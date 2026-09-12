use super::message::Message;

/// Renders what the recipient model actually reads, identically for every transport
/// (Claude channel notification, Stop hook reason, herdr prompt). See docs/protocol.md.
pub struct Envelope;

impl Envelope {
    pub fn render(msg: &Message, from_title: Option<&str>) -> String {
        let title = match from_title.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => format!(" ({t})"),
            None => String::new(),
        };
        let expected = if msg.expects_reply {
            " · reply expected"
        } else {
            ""
        };
        let tail = if msg.expects_reply {
            format!("\n\nReply with agentmail_reply message_id={}", msg.id)
        } else {
            String::new()
        };
        format!(
            "[agentmail] from {}{} · id {}{}\n{}{}",
            msg.from.short_display(),
            title,
            msg.id,
            expected,
            msg.text,
            tail
        )
    }

    /// Several messages drained at once (Stop hook reason).
    pub fn render_batch(msgs: &[Message]) -> String {
        msgs.iter()
            .map(|m| Envelope::render(m, None))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Address, Harness, Message, MessageStatus};
    use crate::ids;

    fn msg(expects_reply: bool) -> Message {
        Message {
            id: "01JXYZ".into(),
            from: Address::new(Harness::Claude, "8890a685-1111-2222-3333-444444444444"),
            to: Address::new(Harness::Codex, "c0de"),
            text: "check the auth middleware".into(),
            reply_to: None,
            expects_reply,
            status: MessageStatus::Pending,
            created_at: ids::now(),
            delivered_at: None,
            pushed_at: None,
            error: None,
        }
    }

    #[test]
    fn renders_the_protocol_envelope() {
        let got = Envelope::render(&msg(true), Some("API authentication"));
        assert_eq!(
            got,
            "[agentmail] from claude:8890a685 (API authentication) · id 01JXYZ · reply expected\n\
             check the auth middleware\n\
             \n\
             Reply with agentmail_reply message_id=01JXYZ"
        );
    }

    #[test]
    fn omits_title_and_reply_line() {
        let got = Envelope::render(&msg(false), None);
        assert_eq!(
            got,
            "[agentmail] from claude:8890a685 · id 01JXYZ\ncheck the auth middleware"
        );
    }

    #[test]
    fn blank_title_is_no_title() {
        let got = Envelope::render(&msg(false), Some("   "));
        assert!(got.starts_with("[agentmail] from claude:8890a685 · id 01JXYZ\n"));
    }
}
