use std::fs;
use std::path::Path;

/// Read the last `lines` lines from a log file.
pub fn tail_log(log_path: &Path, lines: usize) -> String {
    let content = match fs::read_to_string(log_path) {
        Ok(c) => c,
        Err(_) => return "(file not found)".into(),
    };
    let all_lines: Vec<&str> = content.lines().collect();
    let start = all_lines.len().saturating_sub(lines);
    all_lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("teeproxyd_test_{name}_{}", std::process::id()))
    }

    #[test]
    fn tail_nonexistent_file() {
        let result = tail_log(Path::new("/nonexistent/path"), 10);
        assert_eq!(result, "(file not found)");
    }

    #[test]
    fn tail_empty_file() {
        let path = temp_file("empty");
        fs::write(&path, "").unwrap();
        let result = tail_log(&path, 10);
        assert_eq!(result, "");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn tail_fewer_lines_than_requested() {
        let path = temp_file("few");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let result = tail_log(&path, 10);
        assert_eq!(result, "line1\nline2\nline3");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn tail_exact_lines() {
        let path = temp_file("exact");
        fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();
        let result = tail_log(&path, 3);
        assert_eq!(result, "c\nd\ne");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn tail_one_line() {
        let path = temp_file("one");
        fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();
        let result = tail_log(&path, 1);
        assert_eq!(result, "e");
        let _ = fs::remove_file(&path);
    }
}
