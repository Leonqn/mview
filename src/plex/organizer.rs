use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};

/// Replace characters that are invalid in file/directory names on common filesystems.
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

/// Generate the Plex-compatible destination path for a movie file.
///
/// Format: `{movies_dir}/Title (Year)/Title (Year).ext`
pub fn movie_dest_path(
    movies_dir: &str,
    title: &str,
    year: Option<i64>,
    source_file: &str,
) -> PathBuf {
    let ext = Path::new(source_file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mkv");

    let safe_title = sanitize_filename(title);
    let folder_name = match year {
        Some(y) => format!("{safe_title} ({y})"),
        None => safe_title.clone(),
    };

    let file_name = format!("{folder_name}.{ext}");
    PathBuf::from(movies_dir).join(&folder_name).join(file_name)
}

/// Return the Plex series root folder: `{tv_dir}/Title`.
pub fn series_dir_path(tv_dir: &str, title: &str) -> PathBuf {
    PathBuf::from(tv_dir).join(sanitize_filename(title))
}

/// Generate the Plex-compatible destination path for a TV episode file.
///
/// Format: `{tv_dir}/Title/Season XX/Title - SXXEXX - Episode Name.ext`
/// If episode_title is None, omits the episode name part.
pub fn episode_dest_path(
    tv_dir: &str,
    series_title: &str,
    season_number: i64,
    episode_number: i64,
    episode_title: Option<&str>,
    source_file: &str,
) -> PathBuf {
    let ext = Path::new(source_file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mkv");

    let season_folder = format!("Season {:02}", season_number);

    let safe_series = sanitize_filename(series_title);
    let file_name = match episode_title {
        Some(name) if !name.is_empty() => {
            let safe_ep = sanitize_filename(name);
            format!(
                "{} - S{:02}E{:02} - {}.{}",
                safe_series, season_number, episode_number, safe_ep, ext
            )
        }
        _ => {
            format!(
                "{} - S{:02}E{:02}.{}",
                safe_series, season_number, episode_number, ext
            )
        }
    };

    PathBuf::from(tv_dir)
        .join(&safe_series)
        .join(season_folder)
        .join(file_name)
}

/// Organize a single file by creating a hardlink at the destination.
/// Falls back to copying if hardlink fails (e.g. cross-filesystem).
/// Creates parent directories as needed.
pub fn organize_file(source: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    if dest.exists() {
        info!(dest = %dest.display(), "destination already exists, skipping");
        return Ok(());
    }

    match std::fs::hard_link(source, dest) {
        Ok(()) => {
            info!(source = %source.display(), dest = %dest.display(), "hardlinked");
            Ok(())
        }
        Err(error) => {
            warn!(
                ?error,
                source = %source.display(),
                dest = %dest.display(),
                "hardlink failed, falling back to copy"
            );
            std::fs::copy(source, dest).with_context(|| {
                format!("Failed to copy {} -> {}", source.display(), dest.display())
            })?;
            warn!(source = %source.display(), dest = %dest.display(), "copied (fallback, using 2x disk space)");
            Ok(())
        }
    }
}

/// Check if a file has a video extension.
pub fn is_video_file(path: &Path) -> bool {
    let video_extensions = [
        "mkv", "mp4", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "mpg", "mpeg",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| video_extensions.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "sup", "vtt"];
const AUDIO_EXTENSIONS: &[&str] = &["mka", "aac", "ac3", "dts", "flac", "mp3", "ogg", "eac3"];

/// Check if a file is a companion file (subtitle or external audio).
pub fn is_companion_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_lowercase();
            SUBTITLE_EXTENSIONS.contains(&lower.as_str())
                || AUDIO_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Known language folder names mapped to ISO 639-1 codes.
/// Only Russian and English are supported; everything else is ignored.
fn detect_language_from_folder(folder: &str) -> Option<&'static str> {
    match folder.to_lowercase().as_str() {
        "russian" | "rus" | "ru" => Some("ru"),
        "english" | "eng" | "en" => Some("en"),
        _ => None,
    }
}

/// Detect a language tag embedded in the filename (e.g. `01.rus.srt` → "ru").
fn detect_language_from_filename(stem: &str) -> Option<&'static str> {
    stem.rsplit('.')
        .next()
        .and_then(detect_language_from_folder)
}

/// Information extracted from a companion file's path within a torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionInfo {
    /// ISO 639-1 language code if detected ("ru" or "en")
    pub lang: Option<String>,
    /// Group/track label from a parent folder name, e.g. "AniLibria"
    pub label: Option<String>,
}

/// Analyze the relative path of a companion file inside a torrent and extract
/// language code and group label from folder names or filename tags.
///
/// Examples:
///   `Subs/Russian/01.ass`          → lang="ru", label=None
///   `Audio/AniLibria/01.mka`       → lang=None, label="AniLibria"
///   `Audio/Russian AniLibria/01.mka` → lang="ru", label="AniLibria"
///   `01.rus.srt`                   → lang="ru", label=None
pub fn detect_companion_info(relative_path: &str) -> CompanionInfo {
    let path = Path::new(relative_path);
    let mut lang: Option<&str> = None;
    let mut label: Option<String> = None;

    let components: Vec<_> = path
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| {
                    if let std::path::Component::Normal(s) = c {
                        s.to_str()
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let skip = [
        "subs",
        "subtitles",
        "sub",
        "audio",
        "sound",
        "dub",
        "dubs",
        "video",
    ];

    for folder in &components {
        let parts: Vec<&str> = folder.split_whitespace().collect();
        let mut found_lang = false;
        let mut remaining = Vec::new();

        for part in &parts {
            if let Some(code) = detect_language_from_folder(part) {
                lang = Some(code);
                found_lang = true;
            } else if !skip.contains(&part.to_lowercase().as_str()) {
                remaining.push(*part);
            }
        }

        if !found_lang && !skip.contains(&folder.to_lowercase().as_str()) && remaining.is_empty() {
            remaining.push(folder);
        }

        if !remaining.is_empty() {
            label = Some(remaining.join(" "));
        }
    }

    // Fallback: try to detect language from filename (e.g. `01.rus.srt`)
    if lang.is_none()
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
    {
        lang = detect_language_from_filename(stem);
    }

    CompanionInfo {
        lang: lang.map(String::from),
        label: label.map(|s| sanitize_filename(&s)),
    }
}

/// Generate the Plex-compatible destination path for a companion file (subtitle/audio)
/// that should sit next to its corresponding video file.
///
/// Format: `{video_stem}.{lang}.{label}.{ext}` (lang/label omitted if absent)
pub fn companion_dest_path(
    video_dest: &Path,
    companion_source: &str,
    info: &CompanionInfo,
) -> PathBuf {
    let ext = Path::new(companion_source)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("srt");

    let video_stem = video_dest
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let mut name = video_stem.to_string();
    if let Some(ref lang) = info.lang {
        name.push('.');
        name.push_str(lang);
    }
    if let Some(ref lbl) = info.label {
        name.push('.');
        name.push_str(lbl);
    }
    name.push('.');
    name.push_str(ext);

    video_dest.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_movie_dest_path_with_year() {
        let result = movie_dest_path("/media/movies", "Inception", Some(2010), "movie.mkv");
        assert_eq!(
            result,
            PathBuf::from("/media/movies/Inception (2010)/Inception (2010).mkv")
        );
    }

    #[test]
    fn test_movie_dest_path_without_year() {
        let result = movie_dest_path("/media/movies", "Inception", None, "movie.mp4");
        assert_eq!(
            result,
            PathBuf::from("/media/movies/Inception/Inception.mp4")
        );
    }

    #[test]
    fn test_movie_dest_path_preserves_extension() {
        let result = movie_dest_path("/media/movies", "Test", Some(2020), "file.avi");
        assert_eq!(
            result,
            PathBuf::from("/media/movies/Test (2020)/Test (2020).avi")
        );
    }

    #[test]
    fn test_episode_dest_path_with_title() {
        let result = episode_dest_path(
            "/media/tv",
            "Breaking Bad",
            1,
            1,
            Some("Pilot"),
            "episode.mkv",
        );
        assert_eq!(
            result,
            PathBuf::from("/media/tv/Breaking Bad/Season 01/Breaking Bad - S01E01 - Pilot.mkv")
        );
    }

    #[test]
    fn test_episode_dest_path_without_title() {
        let result = episode_dest_path("/media/tv", "Breaking Bad", 2, 5, None, "episode.mp4");
        assert_eq!(
            result,
            PathBuf::from("/media/tv/Breaking Bad/Season 02/Breaking Bad - S02E05.mp4")
        );
    }

    #[test]
    fn test_episode_dest_path_with_empty_title() {
        let result = episode_dest_path("/media/tv", "Show", 1, 3, Some(""), "ep.mkv");
        assert_eq!(
            result,
            PathBuf::from("/media/tv/Show/Season 01/Show - S01E03.mkv")
        );
    }

    #[test]
    fn test_episode_dest_path_double_digit_season() {
        let result = episode_dest_path(
            "/media/tv",
            "Simpsons",
            12,
            15,
            Some("Homer Goes to College"),
            "video.mkv",
        );
        assert_eq!(
            result,
            PathBuf::from(
                "/media/tv/Simpsons/Season 12/Simpsons - S12E15 - Homer Goes to College.mkv"
            )
        );
    }

    #[test]
    fn test_movie_dest_path_sanitizes_special_chars() {
        let result = movie_dest_path(
            "/media/movies",
            "Mission: Impossible",
            Some(1996),
            "movie.mkv",
        );
        assert_eq!(
            result,
            PathBuf::from(
                "/media/movies/Mission_ Impossible (1996)/Mission_ Impossible (1996).mkv"
            )
        );
    }

    #[test]
    fn test_episode_dest_path_sanitizes_special_chars() {
        let result = episode_dest_path(
            "/media/tv",
            "What If...?",
            1,
            1,
            Some("What If... Captain Carter Were the First Avenger?"),
            "ep.mkv",
        );
        let path_str = result.to_string_lossy();
        assert!(!path_str.contains('?'));
        assert!(path_str.contains("What If..._"));
    }

    #[test]
    fn test_organize_file_hardlink() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source.mkv");
        std::fs::write(&source, b"video content").unwrap();

        let dest = tmp.path().join("dest_dir/movie.mkv");
        organize_file(&source, &dest).unwrap();

        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"video content");
    }

    #[test]
    fn test_organize_file_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source.mkv");
        std::fs::write(&source, b"data").unwrap();

        let dest = tmp.path().join("a/b/c/movie.mkv");
        organize_file(&source, &dest).unwrap();

        assert!(dest.exists());
    }

    #[test]
    fn test_organize_file_skips_existing() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source.mkv");
        std::fs::write(&source, b"new data").unwrap();

        let dest = tmp.path().join("existing.mkv");
        std::fs::write(&dest, b"old data").unwrap();

        organize_file(&source, &dest).unwrap();

        // Should not overwrite existing
        assert_eq!(std::fs::read(&dest).unwrap(), b"old data");
    }

    #[test]
    fn test_is_video_file() {
        assert!(is_video_file(Path::new("movie.mkv")));
        assert!(is_video_file(Path::new("movie.mp4")));
        assert!(is_video_file(Path::new("movie.avi")));
        assert!(is_video_file(Path::new("movie.MKV")));
        assert!(!is_video_file(Path::new("movie.txt")));
        assert!(!is_video_file(Path::new("movie.srt")));
        assert!(!is_video_file(Path::new("noext")));
    }

    #[test]
    fn test_is_companion_file() {
        assert!(is_companion_file(Path::new("sub.srt")));
        assert!(is_companion_file(Path::new("sub.ass")));
        assert!(is_companion_file(Path::new("sub.ssa")));
        assert!(is_companion_file(Path::new("sub.vtt")));
        assert!(is_companion_file(Path::new("audio.mka")));
        assert!(is_companion_file(Path::new("audio.ac3")));
        assert!(is_companion_file(Path::new("audio.MKA")));
        assert!(!is_companion_file(Path::new("movie.mkv")));
        assert!(!is_companion_file(Path::new("readme.txt")));
        assert!(!is_companion_file(Path::new("noext")));
    }

    #[test]
    fn test_detect_companion_info_russian_folder() {
        let info = detect_companion_info("Subs/Russian/01.ass");
        assert_eq!(info.lang.as_deref(), Some("ru"));
        assert_eq!(info.label, None);
    }

    #[test]
    fn test_detect_companion_info_english_folder() {
        let info = detect_companion_info("Subtitles/English/ep01.srt");
        assert_eq!(info.lang.as_deref(), Some("en"));
        assert_eq!(info.label, None);
    }

    #[test]
    fn test_detect_companion_info_label_folder() {
        let info = detect_companion_info("Audio/AniLibria/01.mka");
        assert_eq!(info.lang, None);
        assert_eq!(info.label.as_deref(), Some("AniLibria"));
    }

    #[test]
    fn test_detect_companion_info_lang_and_label() {
        let info = detect_companion_info("Audio/Russian AniLibria/01.mka");
        assert_eq!(info.lang.as_deref(), Some("ru"));
        assert_eq!(info.label.as_deref(), Some("AniLibria"));
    }

    #[test]
    fn test_detect_companion_info_filename_lang() {
        let info = detect_companion_info("01.rus.srt");
        assert_eq!(info.lang.as_deref(), Some("ru"));
        assert_eq!(info.label, None);
    }

    #[test]
    fn test_detect_companion_info_filename_eng() {
        let info = detect_companion_info("01.eng.ass");
        assert_eq!(info.lang.as_deref(), Some("en"));
        assert_eq!(info.label, None);
    }

    #[test]
    fn test_detect_companion_info_no_hints() {
        let info = detect_companion_info("01.ass");
        assert_eq!(info.lang, None);
        assert_eq!(info.label, None);
    }

    #[test]
    fn test_detect_companion_info_unknown_lang_ignored() {
        let info = detect_companion_info("Subs/Japanese/01.ass");
        assert_eq!(info.lang, None);
        assert_eq!(info.label.as_deref(), Some("Japanese"));
    }

    #[test]
    fn test_companion_dest_path_with_lang() {
        let video = PathBuf::from("/tv/Show/Season 01/Show - S01E01.mkv");
        let info = CompanionInfo {
            lang: Some("ru".into()),
            label: None,
        };
        let result = companion_dest_path(&video, "sub.srt", &info);
        assert_eq!(
            result,
            PathBuf::from("/tv/Show/Season 01/Show - S01E01.ru.srt")
        );
    }

    #[test]
    fn test_companion_dest_path_with_lang_and_label() {
        let video = PathBuf::from("/tv/Show/Season 01/Show - S01E01.mkv");
        let info = CompanionInfo {
            lang: Some("ru".into()),
            label: Some("AniLibria".into()),
        };
        let result = companion_dest_path(&video, "audio.mka", &info);
        assert_eq!(
            result,
            PathBuf::from("/tv/Show/Season 01/Show - S01E01.ru.AniLibria.mka")
        );
    }

    #[test]
    fn test_companion_dest_path_no_info() {
        let video = PathBuf::from("/tv/Show/Season 01/Show - S01E01.mkv");
        let info = CompanionInfo {
            lang: None,
            label: None,
        };
        let result = companion_dest_path(&video, "sub.ass", &info);
        assert_eq!(
            result,
            PathBuf::from("/tv/Show/Season 01/Show - S01E01.ass")
        );
    }

    #[test]
    fn test_companion_dest_path_label_only() {
        let video = PathBuf::from("/movies/Film (2020)/Film (2020).mkv");
        let info = CompanionInfo {
            lang: None,
            label: Some("AniDUB".into()),
        };
        let result = companion_dest_path(&video, "dub.mka", &info);
        assert_eq!(
            result,
            PathBuf::from("/movies/Film (2020)/Film (2020).AniDUB.mka")
        );
    }
}
