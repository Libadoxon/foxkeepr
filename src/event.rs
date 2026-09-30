//! Parsing of the Hyprland event stream (`.socket2.sock`).
//!
//! Lines look like `eventname>>arg,arg,...`. Addresses arrive without the `0x`
//! prefix that `hyprctl clients` reports, so they are normalised here.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Open {
        addr: String,
        workspace: String,
        class: String,
        title: String,
    },
    Close {
        addr: String,
    },
    Move {
        addr: String,
        workspace: String,
    },
    Title {
        addr: String,
        title: String,
    },
}

/// Canonical window address: lowercase, `0x`-prefixed.
pub fn normalize_addr(raw: &str) -> String {
    let hex = raw.trim().trim_start_matches("0x");
    format!("0x{}", hex.to_ascii_lowercase())
}

pub fn parse(line: &str) -> Option<Event> {
    let (kind, data) = line.split_once(">>")?;
    match kind {
        "openwindow" => {
            // addr,workspace_name,class,title — title may contain commas.
            let mut parts = data.splitn(4, ',');
            Some(Event::Open {
                addr: normalize_addr(parts.next()?),
                workspace: parts.next()?.to_string(),
                class: parts.next()?.to_string(),
                title: parts.next().unwrap_or_default().to_string(),
            })
        }
        "closewindow" => Some(Event::Close {
            addr: normalize_addr(data),
        }),
        "movewindowv2" => {
            // addr,workspace_id,workspace_name — the name is what we target by.
            let mut parts = data.splitn(3, ',');
            let addr = normalize_addr(parts.next()?);
            let _id = parts.next()?;
            Some(Event::Move {
                addr,
                workspace: parts.next()?.to_string(),
            })
        }
        "windowtitlev2" => {
            let (addr, title) = data.split_once(',')?;
            Some(Event::Title {
                addr: normalize_addr(addr),
                title: title.to_string(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_with_commas_in_title() {
        let ev = parse("openwindow>>61bf733d,4,floorp,Foo, bar, baz — Floorp").unwrap();
        assert_eq!(
            ev,
            Event::Open {
                addr: "0x61bf733d".into(),
                workspace: "4".into(),
                class: "floorp".into(),
                title: "Foo, bar, baz — Floorp".into(),
            }
        );
    }

    #[test]
    fn parses_move_to_special_workspace() {
        let ev = parse("movewindowv2>>61bf733d,-98,special:mail").unwrap();
        assert_eq!(
            ev,
            Event::Move {
                addr: "0x61bf733d".into(),
                workspace: "special:mail".into(),
            }
        );
    }

    #[test]
    fn parses_title_and_close() {
        assert_eq!(
            parse("windowtitlev2>>61bf733d,A, B").unwrap(),
            Event::Title {
                addr: "0x61bf733d".into(),
                title: "A, B".into()
            }
        );
        assert_eq!(
            parse("closewindow>>61BF733D").unwrap(),
            Event::Close {
                addr: "0x61bf733d".into()
            }
        );
    }

    #[test]
    fn ignores_unrelated_events() {
        assert!(parse("activewindowv2>>61bf733d").is_none());
        assert!(parse("garbage").is_none());
    }
}
