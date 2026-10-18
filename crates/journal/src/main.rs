#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "claviron-journal is supported only on Linux (systemd/journald required)"
    );
}

#[cfg(target_os = "linux")]
mod linux {
    use std::thread;
    use std::time::Duration;

    use colored::Colorize;
    use systemd::{JournalSeek, journal};
    use time::{OffsetDateTime, format_description};

    /// Followed unless argv or the environment names another. A host that
    /// runs more than one instance — `claviron-dev.service` beside the real
    /// one — needs to say which, and a process started outside systemd
    /// carries no `_SYSTEMD_UNIT` at all, so it never matches any of them.
    const DEFAULT_UNIT: &str = "claviron.service";
    const KEY_UNIT: &str = "_SYSTEMD_UNIT";
    const KEY_MESSAGE: &str = "MESSAGE";
    const KEY_ROLE: &str = "ROLE";
    const KEY_WORKER_ID: &str = "WORKER_ID";
    const KEY_PRIORITY: &str = "PRIORITY";
    const KEY_TARGET: &str = "TARGET";

    pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut args = std::env::args().skip(1);
        let unit = match args.next().as_deref() {
            Some("-h" | "--help") => {
                println!(
                    "usage: claviron-journal [-u UNIT]    (default: {DEFAULT_UNIT})"
                );
                println!("       claviron-journal UNIT");
                println!("       CLAVIRON_UNIT=<unit> claviron-journal");
                return Ok(());
            }
            // `-u` as journalctl spells it: a host whose server does not use
            // the default unit name specifies its actual unit.
            Some("-u" | "--unit") => {
                args.next().ok_or("-u needs a unit name")?
            }
            Some(argument) => argument.to_owned(),
            None => std::env::var("CLAVIRON_UNIT")
                .unwrap_or_else(|_| DEFAULT_UNIT.to_owned()),
        };
        // As with journalctl, a short unit name receives the `.service` suffix.
        let unit =
            if unit.contains('.') { unit } else { format!("{unit}.service") };

        println!("Starting journal-logger for {unit}");

        let mut reader = open_claviron_journal(&unit)?;

        loop {
            loop {
                match reader.next() {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!(
                            "journal next() failed (os={:?}, kind={:?}): {}",
                            err.raw_os_error(),
                            err.kind(),
                            err
                        );
                        return Err(err.into());
                    }
                }

                print_entry(&mut reader)?;
            }

            if let Err(err) = reader.wait(None) {
                eprintln!(
                    "journal wait() failed (os={:?}, kind={:?}): {}",
                    err.raw_os_error(),
                    err.kind(),
                    err
                );

                thread::sleep(Duration::from_secs(1));
                reader = open_claviron_journal(&unit)?;
            }
        }
    }

    fn open_claviron_journal(
        unit: &str
    ) -> Result<journal::Journal, Box<dyn std::error::Error>> {
        let mut reader = journal::OpenOptions::default().open()?;
        reader.match_add(KEY_UNIT, unit)?;
        reader.seek(JournalSeek::Tail)?;
        Ok(reader)
    }

    fn print_entry(
        reader: &mut journal::Journal
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ts = reader.timestamp()?;
        let secs = ts.duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        let odt = OffsetDateTime::from_unix_timestamp(secs)?;
        let local = odt.to_offset(OffsetDateTime::now_local()?.offset());
        let ts_s = local.format(&format_description::parse(
            "[year]-[month]-[day] [hour]:[minute]:[second]",
        )?)?;

        let role = data_string(reader, KEY_ROLE)?;
        let worker_id = data_string(reader, KEY_WORKER_ID)?;
        let message = data_string(reader, KEY_MESSAGE)?;
        let priority = data_string(reader, KEY_PRIORITY)?;
        let _target = data_string(reader, KEY_TARGET)?;

        let level_s = priority_label(priority.as_deref());
        let role_s = role.as_deref().unwrap_or("-");
        let worker_s = worker_id.as_deref().unwrap_or("-");
        let msg_s = message.as_deref().unwrap_or("-");

        let level_colored = match level_s {
            "ERROR" | "CRIT" | "ALERT" | "EMERG" => {
                level_s.red().bold().to_string()
            }
            "WARN" => level_s.yellow().bold().to_string(),
            "INFO" => level_s.green().to_string(),
            "DEBUG" => level_s.blue().to_string(),
            _ => level_s.to_string(),
        };

        let ts_colored = ts_s.bright_white().to_string();
        let role_display = match (role_s, worker_s) {
            ("master", _) => "Master".to_string(),
            ("worker", w) if w != "-" => format!("Worker#{}", w),
            _ => "-".to_string(),
        };
        let worker_colored = role_display.bright_white().to_string();
        let msg_colored = match level_s {
            "DEBUG" => msg_s.dimmed().to_string(),
            "ERROR" => msg_s.bright_red().to_string(),
            _ => msg_s.bright_white().to_string(),
        };

        println!(
            "{} {} {} {}",
            ts_colored, worker_colored, level_colored, msg_colored
        );

        Ok(())
    }

    fn data_string(
        reader: &mut journal::Journal,
        key: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let value = reader.get_data(key).map_err(|err| {
            eprintln!(
                "get_data({}) failed: os={:?} kind={:?} msg={}",
                key,
                err.raw_os_error(),
                err.kind(),
                err
            );
            err
        })?;

        Ok(value.and_then(|value| {
            value
                .value()
                .map(String::from_utf8_lossy)
                .map(|value| value.into_owned())
        }))
    }

    fn priority_label(priority: Option<&str>) -> &'static str {
        match priority {
            Some("0") => "EMERG",
            Some("1") => "ALERT",
            Some("2") => "CRIT",
            Some("3") => "ERROR",
            Some("4") => "WARN",
            Some("5") => "NOTICE",
            Some("6") => "INFO",
            Some("7") => "DEBUG",
            _ => "-",
        }
    }
}
