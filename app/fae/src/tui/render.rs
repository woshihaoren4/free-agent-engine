use super::state::{Message, MessageKind};

fn marker(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::User => ">",
        MessageKind::Assistant => "*",
        MessageKind::Tool => "$",
        MessageKind::Workflow => "+",
        MessageKind::LogError | MessageKind::Error => "!",
        _ => "-",
    }
}
fn heading(message: &Message) -> String {
    let marker = marker(message.kind);
    if message.title.is_empty() { marker.into() } else { format!("{marker} {}", message.title) }
}
pub(super) fn message_heading(message: &Message, include_output_status: bool) -> String {
    let disclosure = if message.is_collapsible() {
        if message.expanded { "[-] " } else { "[+] " }
    } else { "" };
    let output_status = if include_output_status && message.stream_id.is_some() { " Outputting..." } else { "" };
    format!("{disclosure}{}{output_status}", heading(message))
}
pub(super) fn plain_transcript(messages: &[Message]) -> String {
    messages.iter().map(|message| {
        let heading = heading(message);
        if message.content.is_empty() { heading } else {
            format!("{heading}\n{}", message.content.lines().map(|line| format!(" {line}")).collect::<Vec<_>>().join("\n"))
        }
    }).collect::<Vec<_>>().join("\n\n")
}
