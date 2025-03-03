//! This module contains a generic `Tracer`'s methods.
use crate::state::RILL_LINK;
use futures::channel::mpsc;
use meio::Action;
use rill_protocol::data::{self, TimedEvent};
use rill_protocol::io::provider::{Description, Path, Timestamp};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};
use tokio::sync::watch;

#[derive(Debug)]
pub enum DataEnvelope<T: data::Metric> {
    Event(TimedEvent<T::Event>),
}

impl<T: data::Metric> Action for DataEnvelope<T> {}

impl<T: data::Metric> DataEnvelope<T> {
    pub fn into_inner(self) -> TimedEvent<T::Event> {
        match self {
            Self::Event(event) => event,
        }
    }
}

// TODO: Remove that aliases and use raw types receivers in recorders.
pub type DataSender<T> = mpsc::UnboundedSender<DataEnvelope<T>>;
pub type DataReceiver<T> = mpsc::UnboundedReceiver<DataEnvelope<T>>;

pub(crate) enum TracerMode<T: data::Metric> {
    /// Real-time mode
    Push {
        state: T::State,
        receiver: Option<DataReceiver<T>>,
    },
    Pull {
        state: Weak<Mutex<T::State>>,
        interval: Duration,
    },
}

#[derive(Debug)]
enum InnerMode<T: data::Metric> {
    Push { sender: DataSender<T> },
    Pull { state: Arc<Mutex<T::State>> },
}

// TODO: Or require `Clone` for the `Metric` to derive this
impl<T: data::Metric> Clone for InnerMode<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Push { sender } => Self::Push {
                sender: sender.clone(),
            },
            Self::Pull { state } => Self::Pull {
                state: state.clone(),
            },
        }
    }
}

/// The generic provider that forwards metrics to worker and keeps a flag
/// for checking the activitiy status of the `Tracer`.
#[derive(Debug)]
pub struct Tracer<T: data::Metric> {
    /// The receiver that used to activate/deactivate streams.
    active: watch::Receiver<bool>,
    description: Arc<Description>,
    mode: InnerMode<T>,
}

impl<T: data::Metric> Clone for Tracer<T> {
    fn clone(&self) -> Self {
        Self {
            active: self.active.clone(),
            description: self.description.clone(),
            mode: self.mode.clone(),
        }
    }
}

impl<T: data::Metric> Tracer<T> {
    pub(crate) fn new(state: T::State, path: Path, pull: Option<Duration>) -> Self {
        let stream_type = T::stream_type();
        let info = format!("{} - {}", path, stream_type);
        let description = Description {
            path,
            info,
            stream_type,
        };
        // TODO: Remove this active watch channel?
        let (_active_tx, active_rx) = watch::channel(true);
        log::trace!("Creating Tracer with path: {:?}", description.path);
        let description = Arc::new(description);
        let inner_mode;
        let mode;
        if let Some(interval) = pull {
            let state = Arc::new(Mutex::new(state));
            mode = TracerMode::Pull {
                state: Arc::downgrade(&state),
                interval,
            };
            inner_mode = InnerMode::Pull { state };
        } else {
            let (tx, rx) = mpsc::unbounded();
            mode = TracerMode::Push {
                state,
                receiver: Some(rx),
            };
            inner_mode = InnerMode::Push { sender: tx };
        }
        let this = Tracer {
            active: active_rx,
            description: description.clone(),
            mode: inner_mode,
        };
        if let Err(err) = RILL_LINK.register_tracer(description, mode) {
            log::error!(
                "Can't register a Tracer. The worker can be terminated already: {}",
                err
            );
        }
        this
    }

    /// Returns a reference to a `Path` of the `Tracer`.
    pub fn path(&self) -> &Path {
        &self.description.path
    }

    pub(crate) fn send(&self, data: T::Event, opt_system_time: Option<SystemTime>) {
        if self.is_active() {
            let ts = opt_system_time
                .unwrap_or_else(SystemTime::now)
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(Timestamp::from);
            match ts {
                Ok(timestamp) => {
                    let timed_event = TimedEvent {
                        timestamp,
                        event: data,
                    };
                    match &self.mode {
                        InnerMode::Push { sender } => {
                            let envelope = DataEnvelope::Event(timed_event);
                            // And will never send an event
                            if let Err(err) = sender.unbounded_send(envelope) {
                                log::error!("Can't transfer data to sender: {}", err);
                            }
                        }
                        InnerMode::Pull { state } => match state.lock() {
                            Ok(ref mut state) => {
                                T::apply(state, timed_event);
                            }
                            Err(err) => {
                                log::error!("Can't lock the mutex to apply the changes: {}", err);
                            }
                        },
                    }
                }
                Err(err) => {
                    log::error!("Can't make a timestamp from provided system time: {}", err);
                }
            }
        }
    }
}

impl<T: data::Metric> Tracer<T> {
    /// Returns `true` is the `Tracer` has to send data.
    pub fn is_active(&self) -> bool {
        *self.active.borrow()
    }

    /* TODO: Remove or replace with an alternative
    /// Use this method to detect when stream had activated.
    ///
    /// It's useful if you want to spawn async coroutine that
    /// can read a batch of data, but will wait when some streams
    /// will be activated to avoid resources wasting.
    ///
    /// When the generating coroutine active you can use `is_active`
    /// method to detect when to change it to awaiting state again.
    pub async fn when_activated(&mut self) -> Result<(), Error> {
        loop {
            if self.is_active() {
                break;
            }
            self.active.changed().await?;
        }
        Ok(())
    }
    */
}
