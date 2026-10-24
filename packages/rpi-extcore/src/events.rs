//! Replay and live subscription ordering, independent of SSE or ACP.
use crate::host::BufferedEvent;
use futures_util::{
    stream::{self, Stream},
    StreamExt,
};
use tokio::sync::broadcast;

pub enum StreamItem {
    Event(BufferedEvent),
    ResyncRequired { dropped: u64 },
}

pub fn replay_stream(
    replay: Vec<BufferedEvent>,
    receiver: broadcast::Receiver<BufferedEvent>,
    after: Option<u64>,
) -> impl Stream<Item = StreamItem> {
    let cursor = replay
        .last()
        .map(|event| event.seq)
        .unwrap_or_default()
        .max(after.unwrap_or_default());
    stream::iter(replay.into_iter().map(StreamItem::Event)).chain(stream::unfold(
        (receiver, cursor),
        |(mut receiver, cursor)| async move {
            loop {
                match receiver.recv().await {
                    Ok(event) if event.seq <= cursor => continue,
                    Ok(event) => {
                        let cursor = event.seq;
                        return Some((StreamItem::Event(event), (receiver, cursor)));
                    }
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        return Some((StreamItem::ResyncRequired { dropped }, (receiver, cursor)))
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    ))
}

/// A snapshot of the replay boundary, acquired after subscribing to live events.
pub struct Subscription {
    pub ready: serde_json::Value,
    pub replay: Vec<BufferedEvent>,
    pub receiver: broadcast::Receiver<BufferedEvent>,
    pub gap: bool,
    pub after: Option<u64>,
}
impl Subscription {
    pub async fn new(session: &crate::host::HostSession, after: Option<u64>) -> Self {
        let receiver = session.subscribe();
        let retained = session.history(None).await;
        let latest = session.snapshot().await.last_seq;
        let gap =
            crate::service::replay_gap(after, retained.first().map(|event| event.seq), latest);
        let replay = retained
            .into_iter()
            .filter(|event| after.map_or(true, |after| event.seq > after))
            .collect();
        Self {
            ready: session.ready(),
            replay,
            receiver,
            gap,
            after,
        }
    }
}
