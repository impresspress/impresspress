//! One block log record rendered as a single text line, for the loggers that
//! write text rather than structured `tracing` events: the Cloudflare
//! console logger and the browser console logger.
//!
//! The shape matches what `wafer_core`'s `TracingLogger` records as fields:
//! `caller=<block> msg=<text> <key>=<value>…`. The caller is the runtime's
//! registered name for the block that logged (`-` for a record with no
//! attributable caller), so a block cannot pass its lines off as another
//! component's. Keys and values are written by
//! `wafer_core::interfaces::logger::service::RenderedFields`, which quotes any
//! text holding a space, `=` or `"`, so the block's message cannot read as a
//! field of the line. The logger handler has already escaped every control
//! character before the service sees the text; nothing here escapes again.

use std::fmt;

use wafer_core::interfaces::logger::service::{Field, FieldValue, RenderedFields};

/// `Display` adapter for one record; see the module docs for the shape.
pub struct LogLine<'a> {
    pub caller: Option<&'a str>,
    pub msg: &'a str,
    pub fields: &'a [Field],
}

impl fmt::Display for LogLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let head = [
            Field {
                key: "caller".to_string(),
                value: FieldValue::String(self.caller.unwrap_or("-").to_string()),
            },
            Field {
                key: "msg".to_string(),
                value: FieldValue::String(self.msg.to_string()),
            },
        ];
        write!(f, "{}", RenderedFields(&head))?;
        if !self.fields.is_empty() {
            write!(f, " {}", RenderedFields(self.fields))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, value: &str) -> Field {
        Field {
            key: key.to_string(),
            value: FieldValue::String(value.to_string()),
        }
    }

    /// The caller leads the line, and a message that itself spells a field
    /// stays one quoted value.
    #[test]
    fn the_caller_leads_and_the_message_cannot_read_as_a_field() {
        let line = LogLine {
            caller: Some("site/shop"),
            msg: "paid caller=impresspress/admin",
            fields: &[field("order", "o_1")],
        }
        .to_string();
        assert_eq!(
            line,
            r#"caller=site/shop msg="paid caller=impresspress/admin" order=o_1"#
        );
    }

    #[test]
    fn a_record_with_no_caller_says_so() {
        let line = LogLine {
            caller: None,
            msg: "boot",
            fields: &[],
        }
        .to_string();
        assert_eq!(line, "caller=- msg=boot");
    }
}
