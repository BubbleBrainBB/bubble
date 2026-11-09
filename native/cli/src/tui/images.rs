//! Pictures in a prompt, as the TypeScript TUI takes them
//! (`tui/image-paste.ts`): Ctrl+V reads an image from the clipboard and
//! puts a label (`[image#1.png]`) in the composer; an image file's path
//! pasted or dragged in is attached when the prompt is sent, its label in
//! place of the path.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use bubble_core::Image;
use regex::Regex;

/// Larger images are refused (in base64, as the providers count).
const MAX_BASE64: usize = 5 * 1024 * 1024;

fn media_type(extension: &str) -> Option<&'static str> {
    Some(match extension.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return None,
    })
}

fn extension(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/bmp" => ".bmp",
        _ => ".png",
    }
}

fn image(bytes: &[u8], media_type: &str) -> Result<Image, String> {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    if data.len() > MAX_BASE64 {
        return Err(format!("the image is {:.1} MB; images up to about 3.7 MB are sent", bytes.len() as f64 / 1e6));
    }
    Ok(Image { media_type: media_type.to_owned(), data })
}

/// The image at `raw` (`~/…`, shell-escaped spaces allowed).
pub fn read_file(raw: &str) -> Result<Image, String> {
    let unescaped = raw.trim().replace("\\ ", " ");
    let path = match unescaped.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(&unescaped),
    };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let kind = media_type(ext).ok_or_else(|| format!("{} is not an image", path.display()))?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    image(&bytes, kind)
}

/// The image on the clipboard, if there is one.
pub fn clipboard() -> Result<Option<Image>, String> {
    let tmp = std::env::temp_dir().join(format!("bubble_clip_{}.png", bubble_core::time::unique_id()));
    let run = |program: &str, args: &[&str]| {
        let mut command = std::process::Command::new(program);
        command.args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        bubble_core::detach(&mut command);
        command.output().ok().filter(|o| o.status.success())
    };
    if cfg!(target_os = "macos") {
        // Asking for PNG fails when the clipboard holds no image.
        if run("osascript", &["-e", "the clipboard as «class PNGf»"]).is_none() {
            return Ok(None);
        }
        let script = format!(
            "set png_data to (the clipboard as «class PNGf»)\nset fp to open for access POSIX file \"{}\" with write permission\nwrite png_data to fp\nclose access fp",
            tmp.display()
        );
        if run("osascript", &["-e", &script]).is_none() {
            return Ok(None);
        }
        let bytes = std::fs::read(&tmp).map_err(|e| e.to_string());
        let _ = std::fs::remove_file(&tmp);
        return image(&bytes?, "image/png").map(Some);
    }
    for (program, args) in [("wl-paste", &["--type", "image/png"][..]), ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"][..])] {
        if let Some(output) = run(program, args).filter(|o| !o.stdout.is_empty()) {
            return image(&output.stdout, "image/png").map(Some);
        }
    }
    Ok(None)
}

/// The label of the `index`th image.
pub fn label(index: usize, image: &Image) -> String {
    format!("[image#{index}{}]", extension(&image.media_type))
}

/// The prompt to send for `text`: the clipboard images its labels name
/// (from `pasted`), and image paths in it read and replaced by labels.
/// Problems are listed; the prompt goes without those images.
pub fn attach(text: &str, pasted: &HashMap<String, Image>, mut next: usize) -> (String, Vec<Image>, Vec<String>) {
    let mut images = Vec::new();
    let mut problems = Vec::new();
    // Labels, in the order they appear.
    let labels = Regex::new(r"\[image#\d+\.[a-z]+\]").expect("valid");
    let mut found: Vec<(usize, String)> = labels.find_iter(text).map(|m| (m.start(), m.as_str().to_owned())).collect();
    found.dedup_by(|a, b| a.1 == b.1);
    for (_, label) in &found {
        if let Some(image) = pasted.get(label) {
            images.push(image.clone());
        }
    }
    // Paths: absolute or home-relative, quoted or with escaped spaces.
    let paths = Regex::new(r#"(^|\s)(?:"([^"]+\.(?i:png|jpe?g|gif|webp|bmp))"|'([^']+\.(?i:png|jpe?g|gif|webp|bmp))'|((?:~|/)(?:\\ |[^\s"'<>])+\.(?i:png|jpe?g|gif|webp|bmp)))(?:$|\s)"#)
        .expect("valid");
    let mut out = String::new();
    let mut cursor = 0;
    for capture in paths.captures_iter(text) {
        let (Some(whole), Some(raw)) = (capture.get(0), capture.get(2).or(capture.get(3)).or(capture.get(4))) else { continue };
        let lead = capture.get(1).map_or(0, |m| m.len());
        match read_file(raw.as_str()) {
            Ok(image) => {
                next += 1;
                out.push_str(&text[cursor..whole.start() + lead]);
                out.push_str(&label(next, &image));
                // Keep the space the match took after the path.
                if whole.as_str().ends_with(char::is_whitespace) {
                    out.push(' ');
                }
                cursor = whole.end();
                images.push(image);
            }
            Err(problem) => problems.push(problem),
        }
    }
    out.push_str(&text[cursor..]);
    (out, images, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_labels_become_images() {
        let dir = std::env::temp_dir().join(format!("bubble-images-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("my shots")).unwrap();
        let png = dir.join("my shots/a.png");
        std::fs::write(&png, [0x89, b'P', b'N', b'G']).unwrap();
        let escaped = png.display().to_string().replace(' ', "\\ ");
        let mut pasted = HashMap::new();
        pasted.insert("[image#1.png]".to_owned(), Image { media_type: "image/png".into(), data: "AAAA".into() });
        let (text, images, problems) = attach(&format!("compare [image#1.png] with {escaped} please"), &pasted, 1);
        assert_eq!(text, "compare [image#1.png] with [image#2.png] please");
        assert_eq!(images.len(), 2);
        assert_eq!(images[1], Image { media_type: "image/png".into(), data: "iVBORw==".into() });
        assert!(problems.is_empty());
        let (text, images, problems) = attach("look at /nope/missing.jpg", &HashMap::new(), 0);
        assert_eq!((text.as_str(), images.len()), ("look at /nope/missing.jpg", 0));
        assert_eq!(problems.len(), 1);
        assert_eq!(attach("plain words", &HashMap::new(), 0).0, "plain words");
    }
}
