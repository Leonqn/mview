use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::db::models::Notification;
use crate::db::queries;
use crate::plex::organizer;
use crate::telegram::notifications as tg_notify;
use crate::web::AppState;

const POLL_INTERVAL: Duration = Duration::from_secs(60);
const MVIEW_CATEGORY: &str = "mview";

/// Main loop: poll qBittorrent every 60s for completed torrents in the mview category.
pub async fn run(state: Arc<AppState>) {
    info!(
        interval_secs = POLL_INTERVAL.as_secs(),
        "download monitor started"
    );

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    // Skip the immediate first tick to avoid querying qBittorrent on startup
    interval.tick().await;

    loop {
        interval.tick().await;

        if let Err(error) = check_downloads(&state).await {
            error!(?error, "download monitor error");
        }
    }
}

/// Single check iteration: find completed torrents, organize files, update DB, trigger Plex scan.
pub async fn check_downloads(state: &Arc<AppState>) -> Result<()> {
    // Get all active torrents from DB that have a qbt_hash
    let db_torrents = {
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_active_torrents_with_qbt_hash(&conn)
        })
        .await??
    };

    if db_torrents.is_empty() {
        debug!("no active torrents with qbt_hash to monitor");
        return Ok(());
    }

    // Get qBittorrent torrent status (login first to ensure we have a valid session)
    let qbt_torrents = {
        let mut qbt = state.qbittorrent.lock().await;
        qbt.ensure_logged_in().await?;
        qbt.get_torrents(Some(MVIEW_CATEGORY)).await?
    };

    // Build a hash -> qbt_torrent lookup
    let qbt_map: std::collections::HashMap<&str, _> =
        qbt_torrents.iter().map(|t| (t.hash.as_str(), t)).collect();

    let mut scan_paths: Vec<String> = Vec::new();

    for db_torrent in &db_torrents {
        let qbt_hash = match &db_torrent.qbt_hash {
            Some(h) => h.clone(),
            None => continue,
        };

        let qbt_torrent = match qbt_map.get(qbt_hash.as_str()) {
            Some(t) => t,
            None => {
                debug!(
                    title = db_torrent.title,
                    qbt_hash = qbt_hash,
                    "torrent not found in qbittorrent"
                );
                continue;
            }
        };

        // Check if download is complete (progress == 1.0)
        if !is_completed(qbt_torrent.progress, &qbt_torrent.state) {
            debug!(
                title = db_torrent.title,
                progress_pct = format!("{:.1}", qbt_torrent.progress * 100.0).as_str(),
                state = qbt_torrent.state,
                "torrent still downloading"
            );
            continue;
        }

        info!(
            title = db_torrent.title,
            "torrent completed, organizing files"
        );

        // Get file list from qBittorrent
        let files = {
            let mut qbt = state.qbittorrent.lock().await;
            qbt.get_torrent_files(&qbt_hash).await?
        };

        // Process the completed torrent
        match process_completed_torrent(state, db_torrent, &files, &qbt_torrent.save_path).await {
            Ok(dest_path) => {
                scan_paths.push(dest_path);
            }
            Err(error) => {
                error!(
                    title = db_torrent.title,
                    ?error,
                    "failed to process torrent"
                );
                continue;
            }
        }
    }

    // Trigger Plex scan for specific paths
    if !scan_paths.is_empty() {
        trigger_plex_scan(state, &scan_paths).await;
    }

    Ok(())
}

/// Check if a torrent is completed based on progress and state.
pub fn is_completed(progress: f64, state: &str) -> bool {
    // progress 1.0 means fully downloaded
    // States like "uploading", "stalledUP", "pausedUP", "forcedUP", "queuedUP" indicate seeding
    let seeding_states = ["uploading", "stalledUP", "pausedUP", "forcedUP", "queuedUP"];
    let progress_done = (progress - 1.0).abs() < f64::EPSILON;
    // Require full progress for seeding states too, since qBittorrent can report
    // "uploading" during metadata fetch with 0% progress
    progress_done || (seeding_states.contains(&state) && progress >= 0.99)
}

/// Process a single completed torrent: organize files, update DB, create notification.
/// Returns the destination directory path for Plex scanning.
async fn process_completed_torrent(
    state: &Arc<AppState>,
    db_torrent: &crate::db::models::Torrent,
    files: &[crate::qbittorrent::client::QbtTorrentFile],
    save_path: &str,
) -> Result<String> {
    // Load media info for path generation
    let media_id = db_torrent.media_id;
    let pool = state.db.clone();
    let media = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_media(&conn, media_id)
    })
    .await??
    .ok_or_else(|| anyhow::anyhow!("Media {} not found for torrent {}", media_id, db_torrent.id))?;

    let all_video_files: Vec<_> = files
        .iter()
        .filter(|f| organizer::is_video_file(Path::new(&f.name)))
        .collect();
    let video_files = filter_root_video_files(&all_video_files);

    let companion_files: Vec<_> = files
        .iter()
        .filter(|f| organizer::is_companion_file(Path::new(&f.name)))
        .collect();

    if video_files.is_empty() {
        warn!(title = db_torrent.title, "no video files found in torrent");
    }

    // For movie collections, resolve the specific film title from the season
    let movie_title = if media.media_type == "movie" {
        if let Some(season_num) = db_torrent.season_number {
            let media_id = media.id;
            let pool = state.db.clone();
            let seasons = tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                queries::get_seasons_for_media(&conn, media_id)
            })
            .await??;
            seasons
                .iter()
                .find(|s| s.season_number == season_num)
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| media.title.clone())
        } else {
            media.title.clone()
        }
    } else {
        media.title.clone()
    };

    let scan_path = match media.media_type.as_str() {
        "movie" => {
            organize_movie_files(state, &media, &movie_title, &video_files, &companion_files, save_path).await?;
            // Scan the movie folder: {movies_dir}/Title (Year)
            let safe_title = organizer::movie_dest_path(
                &state.config.paths.movies_dir,
                &movie_title,
                media.year,
                "dummy.mkv",
            );
            safe_title
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| state.config.paths.movies_dir.clone())
        }
        "series" | "anime" => {
            let base_dir =
                if media.media_type == "anime" && !state.config.paths.anime_dir.is_empty() {
                    &state.config.paths.anime_dir
                } else {
                    &state.config.paths.tv_dir
                };
            organize_series_files(
                state,
                db_torrent,
                &media,
                &video_files,
                &companion_files,
                save_path,
            )
            .await?;
            let safe_title =
                organizer::episode_dest_path(base_dir, &media.title, 1, 1, None, "dummy.mkv");
            safe_title
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| base_dir.to_string())
        }
        _ => {
            warn!(
                media_type = media.media_type,
                title = db_torrent.title,
                "unknown media type for torrent"
            );
            // Fall back to the root media dirs
            state.config.paths.movies_dir.clone()
        }
    };

    // Mark torrent as completed in DB
    let torrent_id = db_torrent.id;
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::update_torrent_status(&conn, torrent_id, "completed")
    })
    .await??;

    // Disable auto_update if no further monitoring is needed:
    // movies are done after one download; series stop when the season is completed.
    let should_stop_monitoring = match media.media_type.as_str() {
        "movie" => true,
        _ => {
            let season_number = db_torrent.season_number.unwrap_or(1);
            let pool = state.db.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                let seasons = queries::get_seasons_for_media(&conn, media_id)?;
                let done = seasons
                    .iter()
                    .find(|s| s.season_number == season_number)
                    .map(|s| s.status == "completed")
                    .unwrap_or(false);
                Ok::<bool, anyhow::Error>(done)
            })
            .await??
        }
    };
    if should_stop_monitoring {
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::update_torrent_auto_update(&conn, torrent_id, false)
        })
        .await??;
        info!(
            title = db_torrent.title,
            "auto-update disabled, no pending episodes"
        );
    }

    // Create notification
    let notification_msg = format!("Download completed: {}", db_torrent.title);
    let pool = state.db.clone();
    let notif_media_id = Some(media_id);
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::insert_notification(
            &conn,
            &Notification {
                id: 0,
                media_id: notif_media_id,
                message: notification_msg,
                notification_type: "download_complete".to_string(),
                read: false,
                created_at: String::new(),
            },
        )
    })
    .await??;

    // Send Telegram notification
    let tg_msg = tg_notify::format_download_complete(&db_torrent.title);
    if let Err(error) =
        tg_notify::send_notification(&state.telegram_bot, state.telegram_chat_id, &tg_msg).await
    {
        warn!(?error, "failed to send telegram notification");
    }

    info!(title = db_torrent.title, "torrent processed successfully");
    Ok(scan_path)
}

/// Organize movie files into Plex directory structure.
async fn organize_movie_files(
    state: &Arc<AppState>,
    media: &crate::db::models::Media,
    movie_title: &str,
    video_files: &[&crate::qbittorrent::client::QbtTorrentFile],
    companion_files: &[&crate::qbittorrent::client::QbtTorrentFile],
    save_path: &str,
) -> Result<()> {
    let movies_dir = &state.config.paths.movies_dir;

    let mut first_dest: Option<PathBuf> = None;
    for file in video_files {
        let safe_name = sanitize_path(&file.name);
        let source = Path::new(save_path).join(&safe_name);
        let dest = organizer::movie_dest_path(movies_dir, movie_title, media.year, &file.name);

        let source_clone = source.clone();
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || organizer::organize_file(&source_clone, &dest_clone))
            .await??;

        info!(
            source = %source.display(),
            dest = %dest.display(),
            "organized movie file"
        );

        // Organize companion files for this movie
        organize_companions(companion_files, &dest, save_path).await?;

        if first_dest.is_none() {
            first_dest = Some(dest);
        }
    }

    // Mark the placeholder episode as downloaded (movies have one episode as a stub)
    if let Some(dest) = first_dest {
        let media_id = media.id;
        let movie_title = movie_title.to_string();
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let seasons = queries::get_seasons_for_media(&conn, media_id)?;
            // Find the matching season by title (for collections), fall back to first
            let season = seasons
                .iter()
                .find(|s| s.title.as_deref() == Some(&movie_title))
                .or_else(|| seasons.first());
            if let Some(season) = season {
                let episodes = queries::get_episodes_for_season(&conn, season.id)?;
                if let Some(ep) = episodes.first() {
                    let dest_str = dest.to_string_lossy().to_string();
                    queries::update_episode_downloaded(&conn, ep.id, true, Some(&dest_str))?;
                    queries::check_and_complete_season(&conn, season.id)?;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
    }

    Ok(())
}

/// Organize series files into Plex directory structure and update episode records.
async fn organize_series_files(
    state: &Arc<AppState>,
    db_torrent: &crate::db::models::Torrent,
    media: &crate::db::models::Media,
    video_files: &[&crate::qbittorrent::client::QbtTorrentFile],
    companion_files: &[&crate::qbittorrent::client::QbtTorrentFile],
    save_path: &str,
) -> Result<()> {
    let tv_dir = if media.media_type == "anime" && !state.config.paths.anime_dir.is_empty() {
        state.config.paths.anime_dir.clone()
    } else {
        state.config.paths.tv_dir.clone()
    };
    let season_number = db_torrent.season_number.unwrap_or(1);

    // Get episodes for the season from DB
    let media_id = media.id;
    let pool = state.db.clone();
    let seasons = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_seasons_for_media(&conn, media_id)
    })
    .await??;

    let season = seasons.iter().find(|s| s.season_number == season_number);

    let episodes = if let Some(season) = season {
        let season_id = season.id;
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_episodes_for_season(&conn, season_id)
        })
        .await??
    } else {
        Vec::new()
    };

    // Sort video files by extracted episode number, falling back to natural filename order
    let mut sorted_files: Vec<_> = video_files.to_vec();
    sorted_files.sort_by_key(|f| extract_episode_number(&f.name).unwrap_or(i64::MAX));

    // Episodes sorted by episode_number for positional matching
    let mut sorted_episodes: Vec<_> = episodes.iter().collect();
    sorted_episodes.sort_by_key(|e| e.episode_number);

    // If file count matches episode count, map by position. This handles cases like
    // Beatrice-Raws where files 00-12 correspond to DB episodes 1-13.
    let use_positional_mapping = !sorted_episodes.is_empty()
        && sorted_files.len() == sorted_episodes.len()
        && sorted_files
            .iter()
            .all(|f| extract_episode_number(&f.name).is_some());

    for (idx, file) in sorted_files.iter().enumerate() {
        let (episode_number, episode) = if use_positional_mapping {
            let ep = sorted_episodes[idx];
            (ep.episode_number, Some(ep))
        } else {
            let num = extract_episode_number(&file.name).unwrap_or((idx as i64) + 1);
            (num, episodes.iter().find(|e| e.episode_number == num))
        };

        let episode_title = episode.and_then(|e| e.title.as_deref());

        let safe_name = sanitize_path(&file.name);
        let source = Path::new(save_path).join(&safe_name);
        let dest = organizer::episode_dest_path(
            &tv_dir,
            &media.title,
            season_number,
            episode_number,
            episode_title,
            &file.name,
        );

        let source_clone = source.clone();
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || organizer::organize_file(&source_clone, &dest_clone))
            .await??;

        // Update episode downloaded status in DB
        if let Some(ep) = episode {
            let ep_id = ep.id;
            let season_id = ep.season_id;
            let dest_str = dest.to_string_lossy().to_string();
            let pool = state.db.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                queries::update_episode_downloaded(&conn, ep_id, true, Some(&dest_str))?;
                queries::check_and_complete_season(&conn, season_id)?;
                Ok::<_, anyhow::Error>(())
            })
            .await??;
        }

        // Organize companion files matching this episode
        let ep_companions: Vec<_> = companion_files
            .iter()
            .filter(|c| extract_episode_number(&c.name) == Some(episode_number))
            .copied()
            .collect();
        organize_companions(&ep_companions, &dest, save_path).await?;

        info!(
            season = season_number,
            episode = episode_number,
            source = %source.display(),
            dest = %dest.display(),
            "organized episode"
        );
    }

    Ok(())
}

/// Organize companion files (subtitles, external audio) next to a video file.
async fn organize_companions(
    companion_files: &[&crate::qbittorrent::client::QbtTorrentFile],
    video_dest: &Path,
    save_path: &str,
) -> Result<()> {
    for file in companion_files {
        let info = organizer::detect_companion_info(&file.name);
        let safe_name = sanitize_path(&file.name);
        let source = Path::new(save_path).join(&safe_name);
        let dest = organizer::companion_dest_path(video_dest, &file.name, &info);

        let source_clone = source.clone();
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || organizer::organize_file(&source_clone, &dest_clone))
            .await??;

        info!(
            source = %source.display(),
            dest = %dest.display(),
            lang = info.lang.as_deref().unwrap_or("-"),
            label = info.label.as_deref().unwrap_or("-"),
            "organized companion file"
        );
    }
    Ok(())
}

/// Sanitize a file path from an external source (e.g. qBittorrent API) by removing
/// parent directory references (`..`) and absolute path prefixes to prevent path traversal.
fn sanitize_path(path: &str) -> PathBuf {
    let p = Path::new(path);
    let mut result = PathBuf::new();
    for component in p.components() {
        match component {
            Component::Normal(c) => result.push(c),
            Component::CurDir => {}                         // skip "."
            Component::ParentDir => {}                      // skip ".."
            Component::RootDir | Component::Prefix(_) => {} // skip absolute prefixes
        }
    }
    result
}

/// Filter video files to only include those at the shallowest directory level.
/// This excludes files from subfolders like NC/, Extras/, etc.
fn filter_root_video_files<'a>(
    files: &[&'a crate::qbittorrent::client::QbtTorrentFile],
) -> Vec<&'a crate::qbittorrent::client::QbtTorrentFile> {
    if files.is_empty() {
        return Vec::new();
    }
    let min_depth = files
        .iter()
        .map(|f| Path::new(&f.name).components().count())
        .min()
        .unwrap_or(1);
    files
        .iter()
        .filter(|f| Path::new(&f.name).components().count() == min_depth)
        .copied()
        .collect()
}

/// Extract episode number from a filename using common patterns (S01E03, E03, 03, etc.).
/// Returns None if no episode number pattern is found.
fn extract_episode_number(filename: &str) -> Option<i64> {
    // Try S01E03 pattern first using a strict check:
    // Look for 'S' followed by digits followed by 'E' followed by digits
    let upper = filename.to_uppercase();
    let bytes = upper.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'S' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            // Found 'S' + digit, scan past digits to find 'E'
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len()
                && bytes[j] == b'E'
                && j + 1 < bytes.len()
                && bytes[j + 1].is_ascii_digit()
            {
                let after_e = &upper[j + 1..];
                let num_str: String = after_e.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = num_str.parse::<i64>() {
                    return Some(n);
                }
            }
        }
    }

    // Try "Episode XX" or "Ep XX" pattern
    let lower = filename.to_lowercase();
    for prefix in &["episode ", "episode_", "episode.", "ep ", "ep_", "ep."] {
        if let Some(pos) = lower.find(prefix) {
            let after = &lower[pos + prefix.len()..];
            let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = num_str.parse::<i64>() {
                return Some(n);
            }
        }
    }

    // Try " - XX" or ".XX." pattern with standalone numbers
    // Extract the last group of digits before the file extension as a fallback
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    // Find digit groups, prefer the last one that looks like an episode number (1-999).
    // Skip known non-episode numbers (resolutions like 720p, 1080p, 480p, 2160p).
    let resolution_numbers: &[i64] = &[240, 360, 480, 720, 1080, 2160, 4320];
    let mut last_num = None;
    let chars_vec: Vec<char> = stem.chars().collect();
    let mut i = 0;
    while i < chars_vec.len() {
        if chars_vec[i].is_ascii_digit() {
            let mut num_str = String::new();
            while i < chars_vec.len() && chars_vec[i].is_ascii_digit() {
                num_str.push(chars_vec[i]);
                i += 1;
            }
            // Check if followed by 'p' or 'i' (resolution indicator like 720p, 1080i)
            let is_resolution = i < chars_vec.len()
                && (chars_vec[i] == 'p'
                    || chars_vec[i] == 'P'
                    || chars_vec[i] == 'i'
                    || chars_vec[i] == 'I');
            // Check if preceded by 'x' or 'h' (codec like x264, x265, h264, h265)
            let start_idx = i - num_str.len();
            let is_codec = start_idx > 0
                && (chars_vec[start_idx - 1] == 'x'
                    || chars_vec[start_idx - 1] == 'X'
                    || chars_vec[start_idx - 1] == 'h'
                    || chars_vec[start_idx - 1] == 'H');
            if let Ok(n) = num_str.parse::<i64>()
                && (0..1000).contains(&n)
                && !is_resolution
                && !is_codec
                && !resolution_numbers.contains(&n)
            {
                last_num = Some(n);
            }
        } else {
            i += 1;
        }
    }
    last_num
}

/// Trigger Plex library scan for specific paths, logging errors without failing.
async fn trigger_plex_scan(state: &Arc<AppState>, paths: &[String]) {
    crate::plex::client::scan(state, paths).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_completed_by_progress() {
        assert!(is_completed(1.0, "downloading"));
        assert!(!is_completed(0.5, "downloading"));
        assert!(!is_completed(0.99, "downloading"));
    }

    #[test]
    fn test_is_completed_by_state() {
        // Seeding states with full progress are completed
        assert!(is_completed(1.0, "uploading"));
        assert!(is_completed(1.0, "stalledUP"));
        assert!(is_completed(1.0, "pausedUP"));
        assert!(is_completed(1.0, "forcedUP"));
        assert!(is_completed(1.0, "queuedUP"));
        // Seeding states with 0% progress are NOT completed (metadata fetch stage)
        assert!(!is_completed(0.0, "uploading"));
        assert!(!is_completed(0.0, "stalledUP"));
        assert!(!is_completed(0.0, "downloading"));
        assert!(!is_completed(0.0, "stalledDL"));
    }

    #[test]
    fn test_is_completed_seeding() {
        // Typical completed torrent: progress 1.0 and seeding state
        assert!(is_completed(1.0, "uploading"));
        assert!(is_completed(1.0, "stalledUP"));
    }

    #[tokio::test]
    async fn test_check_downloads_no_active_torrents() {
        // This tests the early return when there are no active torrents with qbt_hash.
        // We need a real AppState for this, so we construct one with an in-memory DB.
        use crate::config::{
            Config, PathsConfig, PlexConfig, QbittorrentConfig, RutrackerConfig, ServerConfig,
            TelegramConfig, TmdbConfig,
        };
        use crate::db;
        use crate::qbittorrent::client::QbtClient;
        use crate::rutracker::client::RutrackerClient;
        use crate::tmdb::client::TmdbClient;
        use crate::web::AppState;

        let pool = db::init_pool(":memory:").unwrap();

        let config = Config {
            rutracker: RutrackerConfig {
                url: "http://127.0.0.1:19999".to_string(),
                username: "user".to_string(),
                password: "pass".to_string(),
            },
            qbittorrent: QbittorrentConfig {
                url: "http://localhost:8080".to_string(),
                username: "admin".to_string(),
                password: "admin".to_string(),
            },
            tmdb: TmdbConfig {
                api_key: "fake".to_string(),
            },
            plex: PlexConfig::default(),
            telegram: TelegramConfig::default(),
            paths: PathsConfig {
                download_dir: "/tmp/downloads".to_string(),
                movies_dir: "/tmp/movies".to_string(),
                tv_dir: "/tmp/tv".to_string(),
                anime_dir: "/tmp/anime".to_string(),
            },
            server: ServerConfig::default(),
        };

        let rt_config = Arc::new(config.rutracker.clone());
        let auth_handle = crate::rutracker::auth::spawn_auth_task(rt_config);
        let rt_client = RutrackerClient::new(&config.rutracker.url, auth_handle.clone());
        let tmdb_client = TmdbClient::new(&config.tmdb.api_key).unwrap();
        let qbt_config = Arc::new(config.qbittorrent.clone());
        let qbt_client = QbtClient::new(qbt_config).unwrap();

        let state = Arc::new(AppState {
            db: pool,
            rutracker: rt_client,
            tmdb: tmdb_client,
            anilist: crate::anilist::client::AniListClient::new().unwrap(),
            qbittorrent: tokio::sync::Mutex::new(qbt_client),
            auth_handle,
            telegram_bot: teloxide::Bot::new("fake:token"),
            telegram_chat_id: 0,
            config,
            templates: crate::web::init_templates(),
        });

        // With empty DB, should return Ok without errors
        let result = check_downloads(&state).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_db_query_active_torrents_with_qbt_hash() {
        use crate::db;
        use crate::db::models::{Media, Torrent};
        use crate::db::queries;

        let pool = db::init_pool(":memory:").unwrap();
        let conn = pool.get().unwrap();

        // Insert a media
        let media = Media {
            id: 0,
            media_type: "series".to_string(),
            title: "Test Series".to_string(),
            title_original: None,
            year: Some(2024),
            tmdb_id: Some(12345),
            imdb_id: None,
            kinopoisk_url: None,
            world_art_url: None,
            poster_url: None,
            overview: None,
            anilist_id: None,
            status: "tracking".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let media_id = queries::insert_media(&conn, &media).unwrap();

        // Insert torrent with qbt_hash (active)
        let torrent = Torrent {
            id: 0,
            media_id,
            rutracker_topic_id: "123456".to_string(),
            title: "Test.Torrent.S01".to_string(),
            quality: Some("1080p".to_string()),
            size_bytes: Some(5_000_000_000),
            seeders: Some(10),
            season_number: Some(1),
            episode_info: Some("1-12".to_string()),
            registered_at: None,
            last_checked_at: None,
            torrent_hash: None,
            qbt_hash: Some("abcdef123456".to_string()),
            status: "active".to_string(),
            auto_update: true,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let torrent_id = queries::insert_torrent(&conn, &torrent).unwrap();

        // Insert torrent without qbt_hash (should not appear)
        let torrent2 = Torrent {
            qbt_hash: None,
            title: "No.Hash.Torrent".to_string(),
            rutracker_topic_id: "789".to_string(),
            ..torrent.clone()
        };
        queries::insert_torrent(&conn, &torrent2).unwrap();

        // Insert completed torrent with qbt_hash (should not appear)
        let torrent3 = Torrent {
            qbt_hash: Some("completed123".to_string()),
            status: "completed".to_string(),
            title: "Completed.Torrent".to_string(),
            rutracker_topic_id: "456".to_string(),
            ..torrent.clone()
        };
        queries::insert_torrent(&conn, &torrent3).unwrap();

        let active = queries::get_active_torrents_with_qbt_hash(&conn).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, torrent_id);
        assert_eq!(active[0].qbt_hash, Some("abcdef123456".to_string()));

        // Test update_torrent_status
        queries::update_torrent_status(&conn, torrent_id, "completed").unwrap();
        let active = queries::get_active_torrents_with_qbt_hash(&conn).unwrap();
        assert_eq!(active.len(), 0);
    }

    #[test]
    fn test_extract_episode_number_sxxexx() {
        assert_eq!(extract_episode_number("Show.S01E03.1080p.mkv"), Some(3));
        assert_eq!(extract_episode_number("show.s02e15.720p.mkv"), Some(15));
    }

    #[test]
    fn test_extract_episode_number_episode_keyword() {
        assert_eq!(extract_episode_number("Show Episode 5.mkv"), Some(5));
        assert_eq!(extract_episode_number("show.ep.03.mkv"), Some(3));
    }

    #[test]
    fn test_extract_episode_number_skips_resolution() {
        // Should not pick up 720 or 1080 as episode numbers
        assert_eq!(extract_episode_number("Show.720p.mkv"), None);
        assert_eq!(extract_episode_number("Show.1080p.mkv"), None);
        assert_eq!(extract_episode_number("Show.2160p.mkv"), None);
    }

    #[test]
    fn test_extract_episode_number_fallback_digit() {
        // Falls back to last digit group that isn't a resolution
        assert_eq!(extract_episode_number("Show.05.mkv"), Some(5));
    }

    #[test]
    fn test_extract_episode_number_zero() {
        assert_eq!(
            extract_episode_number(
                "[Beatrice-Raws] Fate Stay Night - Unlimited Blade Works 00 [BDRip 1920x1080 HEVC TrueHD].mkv"
            ),
            Some(0)
        );
        assert_eq!(extract_episode_number("Show.S01E00.Special.mkv"), Some(0));
        assert_eq!(extract_episode_number("Show Episode 0.mkv"), Some(0));
    }

    #[test]
    fn test_extract_episode_number_none() {
        assert_eq!(extract_episode_number("Show.mkv"), None);
    }

    #[test]
    fn test_sanitize_path_normal() {
        assert_eq!(
            sanitize_path("foo/bar/baz.mkv"),
            PathBuf::from("foo/bar/baz.mkv")
        );
    }

    #[test]
    fn test_sanitize_path_traversal() {
        assert_eq!(
            sanitize_path("../../etc/passwd"),
            PathBuf::from("etc/passwd")
        );
        assert_eq!(
            sanitize_path("../foo/../bar/file.txt"),
            PathBuf::from("foo/bar/file.txt")
        );
    }

    #[test]
    fn test_sanitize_path_absolute() {
        assert_eq!(sanitize_path("/etc/passwd"), PathBuf::from("etc/passwd"));
    }

    #[test]
    fn test_sanitize_path_current_dir() {
        assert_eq!(
            sanitize_path("./foo/./bar.mkv"),
            PathBuf::from("foo/bar.mkv")
        );
    }
}
