use chrono::Local;
use std::path::Path;

/// Fills `${placeholder}` tokens in prompts.
///
/// Supported: `${date}` (YYYY-MM-DD), `${time}` (HH:MM), `${datetime}`,
/// `${weekday}`, `${holidays}` (comma-joined config list), `${task}`,
/// `${workstation}` (the task's scratch directory).
pub fn render(text: &str, task: &str, workstation: &Path, holidays: &[String]) -> String {
    let now = Local::now();
    let pairs = [
        ("${date}", now.format("%Y-%m-%d").to_string()),
        ("${time}", now.format("%H:%M").to_string()),
        ("${datetime}", now.format("%Y-%m-%d %H:%M %Z").to_string()),
        ("${weekday}", now.format("%A").to_string()),
        ("${holidays}", holidays.join(", ")),
        ("${task}", task.to_string()),
        ("${workstation}", workstation.to_string_lossy().into_owned()),
    ];
    pairs.iter().fold(text.to_string(), |acc, (k, v)| acc.replace(k, v))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn fills_placeholders() {
        let out = super::render(
            "t=${task} w=${workstation} h=${holidays} d=${date}",
            "x",
            Path::new("/ws/x"),
            &["2026-12-25".into(), "2027-01-01".into()],
        );
        assert!(out.starts_with("t=x w=/ws/x h=2026-12-25, 2027-01-01 d=20"));
    }
}
