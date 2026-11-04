//! The tray (docs/07 §7): a `TrayModel` actor per active context, fed by the bridge.
//!
//! * [`model`]: the pure model (snapshot + event reducer, aggregate state, the menu as data,
//!   tooltip, actions).
//! * [`icons`]: the embedded state icons (DPI size, taskbar theme, remote overlay) and the
//!   monochrome menu glyphs.
//! * [`menu`]: the native side (Tauri tray icon and menu, dialogs, window navigation).
//!
//! The actor ([`Tray::spawn`], ADR-0011) owns the model. It follows the bridge's state of
//! its context, holds its **own** `events.subscribe` through the bridge's `Contexts`
//! ([`TrayBackend`]), fetches the snapshot (`services.status`, `sites.list`, `php.list`)
//! once per connection and again only after a `reset`/`lagged` entry, a lost frame or an
//! event that asks for it — never by polling. Renders are debounced: at most one per
//! [`TrayOptions::debounce`] (100 ms; a change after a quiet period renders at once, a burst
//! is coalesced), and none while the menu is open (the rebuild waits for it to close). Every daemon call has a deadline and runs in a task of its own: the
//! actor never awaits I/O while handling a message.

pub mod icons;
pub mod menu;
pub mod model;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::AbortHandle;

use forge_ipc::ApiError;
use forge_ipc::api::{MailStatusDto, PhpVersionListDto, ServiceStatusDto, SiteListDto};
use forge_ipc::base::actor::{
    Actor, ActorHandle, ActorRef, ActorSpec, ActorSupervisor, AskError, Context, Reply,
    WeakActorRef,
};
use forge_ipc::base::event::EventBus;
use forge_ipc::base::ids::RequestId;

use crate::bridge::{
    Bridge, CallArgs, ConnectionStatus, ContextStateDto, FrameSink, SinkClosed, StreamEntry,
    StreamFrame, SubscribeAnswer,
};
use crate::i18n;
use crate::notifications::{NoticeEngine, NoticeKind, Notices, Toast};

use self::icons::{IconKey, STARTING_FRAMES, icon_size_for_scale};
use self::model::{Effect, Link, MenuSpec, Scope, Snapshot, TrayAction, TrayModel, TrayState};

/// Owner label of the tray's stream in the bridge (page reloads close `main`'s only).
pub const STREAM_OWNER: &str = "tray";

/// What the tray needs from the bridge (the `Contexts` actor). [`Bridge`] in the app; a
/// recording fake in tests.
pub trait TrayBackend: Send + Sync + 'static {
    /// One API call (`ipc_call` semantics: validated, `request_id`, deadline).
    fn call(&self, args: CallArgs) -> BoxFuture<'static, Result<Value, ApiError>>;
    /// The state of `context` (opens it on first use).
    fn state(&self, context: &str) -> BoxFuture<'static, Result<ContextStateDto, ApiError>>;
    /// Every state change of every context.
    fn watch_states(&self) -> broadcast::Receiver<ContextStateDto>;
    /// Opens a stream of `events.subscribe` on `context` whose frames go to `sink`.
    fn subscribe_events(
        &self,
        context: &str,
        params: Value,
        sink: Arc<dyn FrameSink>,
    ) -> BoxFuture<'static, Result<SubscribeAnswer, ApiError>>;
    /// The tray handled `frame` of `stream_id`.
    fn ack(&self, stream_id: &str, frame: u64);
    /// Closes a stream opened by [`TrayBackend::subscribe_events`].
    fn unsubscribe(&self, context: &str, stream_id: &str);
}

impl TrayBackend for Bridge {
    fn call(&self, args: CallArgs) -> BoxFuture<'static, Result<Value, ApiError>> {
        let bridge = self.clone();
        async move { Bridge::call(&bridge, args).await }.boxed()
    }

    fn state(&self, context: &str) -> BoxFuture<'static, Result<ContextStateDto, ApiError>> {
        let (bridge, context) = (self.clone(), context.to_owned());
        async move { Bridge::state(&bridge, &context).await }.boxed()
    }

    fn watch_states(&self) -> broadcast::Receiver<ContextStateDto> {
        Bridge::watch_states(self)
    }

    fn subscribe_events(
        &self,
        context: &str,
        params: Value,
        sink: Arc<dyn FrameSink>,
    ) -> BoxFuture<'static, Result<SubscribeAnswer, ApiError>> {
        let (bridge, context) = (self.clone(), context.to_owned());
        async move {
            Bridge::subscribe(
                &bridge,
                &context,
                "events.subscribe",
                Some(params),
                sink,
                STREAM_OWNER,
            )
            .await
        }
        .boxed()
    }

    fn ack(&self, stream_id: &str, frame: u64) {
        Bridge::ack(self, stream_id, frame);
    }

    fn unsubscribe(&self, context: &str, stream_id: &str) {
        let _ = Bridge::unsubscribe(self, context, stream_id);
    }
}

/// What the tray asks of the native side ([`menu::NativeView`]; a recorder in tests).
/// Every method returns at once (the native side posts to the main thread).
pub trait TrayView: Send + Sync + 'static {
    /// Shows a new menu, tooltip and icon.
    fn render(&self, render: &TrayRender);
    /// Only the icon changed (a frame of the `starting` ring).
    fn set_icon(&self, icon: IconKey);
    /// Opens or focuses the main window.
    fn show_window(&self);
    /// Shows the window on `route` (`/logs`, `/settings`...).
    fn navigate(&self, route: &str);
    /// Opens a site in the user's browser (client-side: the daemon may be remote).
    fn open_url(&self, url: &str);
    /// Opens the global terminal (the webview runs `terminal.open`).
    fn open_terminal(&self);
    /// Asks whether to stop the services before quitting.
    fn ask_quit(&self) -> BoxFuture<'static, QuitChoice>;
    /// Exits the GUI process.
    fn exit(&self);
}

/// One render of the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayRender {
    /// 1, 2, 3... per actor.
    pub revision: u64,
    pub menu: MenuSpec,
    pub tooltip: String,
    pub icon: IconKey,
    /// The menu glyphs are drawn for a dark menu.
    pub dark_menu: bool,
}

/// What the native side knows of the screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrayAppearance {
    /// `SystemAppearance.taskbar_light`: the icon variant.
    pub taskbar_light: bool,
    /// Windows popup menus follow the system app mode (see docs/07 §7).
    pub dark_menu: bool,
    /// Scale factor of the taskbar's monitor (1.0 = 96 DPI): the icon size.
    pub scale: f64,
    /// `prefers-reduced-motion` (reported by the webview): a static `starting` frame.
    pub reduced_motion: bool,
}

impl Default for TrayAppearance {
    fn default() -> Self {
        Self {
            taskbar_light: false,
            dark_menu: false,
            scale: 1.0,
            reduced_motion: false,
        }
    }
}

/// A click on the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Left button: open or focus the window.
    Left,
    /// Middle button: the global terminal, when implemented.
    Middle,
    /// The context menu is about to show (right button down).
    MenuOpening,
    /// The menu is gone (an entry was picked, or the pointer is back on the icon).
    MenuClosed,
}

/// The answer to "Sair".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitChoice {
    /// `daemon.stop {keep_services: false}`, then exit.
    StopServices,
    /// Exit the GUI only; the daemon and the services keep running.
    KeepServices,
    Cancel,
}

/// What the quit flow did.
#[derive(Debug, Clone, PartialEq)]
pub enum QuitOutcome {
    Cancelled,
    /// Exited without calling the daemon.
    Exited,
    /// Exited after `daemon.stop` answered (`Ok`), failed or timed out (`Err`).
    StoppedAndExited(Result<(), ApiError>),
}

/// Tuning.
#[derive(Debug, Clone, Copy)]
pub struct TrayOptions {
    /// A change re-renders at most this late.
    pub debounce: Duration,
    /// Deadline of each snapshot call.
    pub call_timeout: Duration,
    /// Deadline of an action (jobs answer with a `job_id` at once).
    pub action_timeout: Duration,
    /// `daemon.stop` of the quit flow.
    pub quit_timeout: Duration,
    /// Frame time of the `starting` ring (~8 fps).
    pub frame_interval: Duration,
    /// A menu reported open is treated as closed after this long.
    pub menu_open_timeout: Duration,
    /// Deadline of a question to the actor.
    pub ask_timeout: Duration,
    /// A failed snapshot (or refused subscription) is tried again after this long, doubling
    /// while it keeps failing (at most a minute).
    pub retry_after: Duration,
}

impl Default for TrayOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(100),
            call_timeout: Duration::from_secs(10),
            action_timeout: Duration::from_secs(15),
            quit_timeout: Duration::from_secs(5),
            frame_interval: Duration::from_millis(125),
            menu_open_timeout: Duration::from_secs(5),
            ask_timeout: Duration::from_secs(2),
            retry_after: Duration::from_secs(3),
        }
    }
}

/// The tray actor's address. Cheap to clone.
#[derive(Clone)]
pub struct Tray {
    inner: Arc<TrayInner>,
}

struct TrayInner {
    handle: ActorHandle<Msg>,
    _supervisor: ActorSupervisor,
    ask_timeout: Duration,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tray").finish_non_exhaustive()
    }
}

impl Tray {
    /// Starts the actor for `context`. It stays idle (no connection, no call) until
    /// [`Tray::activate`]: the shell paints first (docs/07, ADR-0034).
    pub fn spawn(
        backend: Arc<dyn TrayBackend>,
        view: Arc<dyn TrayView>,
        context: &str,
        options: TrayOptions,
        appearance: TrayAppearance,
    ) -> Self {
        Self::spawn_with_notices(backend, view, context, options, appearance, None)
    }

    /// [`Tray::spawn`] plus native toasts (M2-E1-T16, [`crate::notifications`]): the actor
    /// feeds its events to a [`NoticeEngine`] and shows what it returns through `notices`.
    pub fn spawn_with_notices(
        backend: Arc<dyn TrayBackend>,
        view: Arc<dyn TrayView>,
        context: &str,
        options: TrayOptions,
        appearance: TrayAppearance,
        notices: Option<Notices>,
    ) -> Self {
        let supervisor = ActorSupervisor::new(EventBus::new());
        let context = context.to_owned();
        let handle = supervisor.spawn(ActorSpec::new("desktop.tray").mailbox(1024), move || {
            TrayActor::new(
                Arc::clone(&backend),
                Arc::clone(&view),
                &context,
                options,
                appearance,
                notices.clone(),
            )
        });
        Self {
            inner: Arc::new(TrayInner {
                handle,
                _supervisor: supervisor,
                ask_timeout: options.ask_timeout,
            }),
        }
    }

    fn actor(&self) -> &ActorRef<Msg> {
        self.inner.handle.actor_ref()
    }

    fn tell(&self, msg: Msg) {
        if let Err(err) = self.actor().tell(msg) {
            tracing::warn!(%err, "tray actor did not take a message");
        }
    }

    /// Connects: follows the context, subscribes to its events, fetches the snapshot.
    pub fn activate(&self) {
        self.tell(Msg::Activate);
    }

    /// A menu entry was picked (its id, see [`TrayAction::id`]).
    pub fn menu_event(&self, id: &str) {
        self.tell(Msg::Action(id.to_owned()));
    }

    pub fn click(&self, click: Click) {
        self.tell(Msg::Click(click));
    }

    pub fn set_appearance(&self, appearance: TrayAppearance) {
        self.tell(Msg::Appearance(appearance));
    }

    pub fn set_reduced_motion(&self, reduced: bool) {
        self.tell(Msg::ReducedMotion(reduced));
    }

    /// The UI language changed (`set_language`): re-render the labels.
    pub fn language_changed(&self) {
        self.tell(Msg::Language);
    }

    /// The app updater announced (or cleared) an update.
    /// The user clicked a toast: focus the window and open the related screen.
    pub fn notification_clicked(&self, kind: NoticeKind) {
        self.tell(Msg::NoticeClicked(kind));
    }

    pub fn set_app_update(&self, available: bool) {
        self.tell(Msg::AppUpdate(available));
    }

    /// The model now (tests, diagnostics).
    pub async fn model(&self) -> Result<TrayModel, AskError> {
        self.actor().ask(Msg::Model, self.inner.ask_timeout).await
    }

    /// Closes the stream and stops the actor.
    pub async fn stop(&self) {
        let _ = self
            .actor()
            .ask(Msg::Shutdown, self.inner.ask_timeout)
            .await;
        let _ = self.inner.handle.stop().await;
    }
}

/// Messages of the tray actor.
pub enum Msg {
    Activate,
    Connection(ContextStateDto),
    /// The state broadcast lagged: ask the current state again.
    StatesLagged,
    StateAnswer {
        epoch: u64,
        result: Result<ContextStateDto, ApiError>,
    },
    Subscribed {
        epoch: u64,
        result: Result<SubscribeAnswer, ApiError>,
    },
    Frame {
        epoch: u64,
        frame: StreamFrame,
    },
    Snapshot {
        epoch: u64,
        snapshot: Snapshot,
        failed: bool,
    },
    Flush,
    Tick {
        token: u64,
    },
    MenuTimeout {
        token: u64,
    },
    Retry {
        epoch: u64,
    },
    Action(String),
    Click(Click),
    Appearance(TrayAppearance),
    ReducedMotion(bool),
    Language,
    AppUpdate(bool),
    NoticeClicked(NoticeKind),
    /// The grouping window of some toast type may have ended.
    NoticeFlush,
    QuitDone(QuitOutcome),
    Model(Reply<TrayModel>),
    Shutdown(Reply<()>),
}

/// Sends `msg` to the actor from one of its tasks; gives up when it is gone.
fn tell(weak: &WeakActorRef<Msg>, msg: Msg) {
    if let Some(actor) = weak.upgrade() {
        if let Err(err) = actor.tell(msg) {
            tracing::warn!(%err, "tray actor did not take a message");
        }
    }
}

/// Frames of the tray's stream, straight into the actor's mailbox. A frame the mailbox
/// cannot take is counted as lost: the actor fetches the snapshot again.
struct TraySink {
    actor: WeakActorRef<Msg>,
    backend: Arc<dyn TrayBackend>,
    epoch: u64,
    lost: Arc<AtomicBool>,
}

impl FrameSink for TraySink {
    fn send(&self, frame: StreamFrame) -> Result<(), SinkClosed> {
        let Some(actor) = self.actor.upgrade() else {
            return Err(SinkClosed);
        };
        self.backend.ack(&frame.stream_id, frame.frame);
        match actor.tell(Msg::Frame {
            epoch: self.epoch,
            frame,
        }) {
            Ok(()) => Ok(()),
            Err(err) if err.is_full() => {
                self.lost.store(true, Ordering::SeqCst);
                Ok(())
            }
            Err(_) => Err(SinkClosed),
        }
    }
}

struct TrayActor {
    backend: Arc<dyn TrayBackend>,
    view: Arc<dyn TrayView>,
    options: TrayOptions,
    appearance: TrayAppearance,
    model: TrayModel,
    active: bool,
    /// Grows when the context changes: answers of an older one are dropped.
    epoch: u64,
    stream: Option<String>,
    subscribing: bool,
    snapshot_inflight: bool,
    /// Asked for while a fetch was in flight (or the link was down).
    pending_scope: Scope,
    /// Events received while a fetch was in flight: applied after it.
    queued: Vec<(String, Value)>,
    lost: Arc<AtomicBool>,
    flush_scheduled: bool,
    menu_open: bool,
    menu_token: u64,
    /// A render waits for the menu to close.
    dirty: bool,
    last: Option<TrayRender>,
    revision: u64,
    frame: u8,
    ticking: bool,
    tick_token: u64,
    retry_scheduled: bool,
    /// Consecutive failed retries (the delay doubles).
    retry_failures: u32,
    quitting: bool,
    states_task: Option<AbortHandle>,
    /// The actor's own address, for its timers (set in `on_start`).
    weak: Option<WeakActorRef<Msg>>,
    last_render_at: Option<tokio::time::Instant>,
    /// Native toasts (None: the tray runs without them).
    notices: Option<(NoticeEngine, Notices)>,
    /// Connection number the toast cursor was taken for.
    notice_generation: Option<u64>,
    /// A timer for the engine's next window end is pending.
    notice_timer: bool,
}

impl TrayActor {
    fn new(
        backend: Arc<dyn TrayBackend>,
        view: Arc<dyn TrayView>,
        context: &str,
        options: TrayOptions,
        appearance: TrayAppearance,
        notices: Option<Notices>,
    ) -> Self {
        Self {
            backend,
            view,
            options,
            appearance,
            model: TrayModel::new(context),
            active: false,
            epoch: 1,
            stream: None,
            subscribing: false,
            snapshot_inflight: false,
            pending_scope: Scope::default(),
            queued: Vec::new(),
            lost: Arc::new(AtomicBool::new(false)),
            flush_scheduled: false,
            menu_open: false,
            menu_token: 0,
            dirty: false,
            last: None,
            revision: 0,
            frame: 0,
            ticking: false,
            tick_token: 0,
            retry_scheduled: false,
            retry_failures: 0,
            quitting: false,
            states_task: None,
            weak: None,
            last_render_at: None,
            notices: notices.map(|n| {
                let mut engine = NoticeEngine::with_window(Arc::clone(&n.mutes), n.window);
                // No toast before the first `hello` fixes the cursor.
                engine.await_cursor();
                (engine, n)
            }),
            notice_generation: None,
            notice_timer: false,
        }
    }
}

impl Actor for TrayActor {
    type Msg = Msg;

    async fn on_start(&mut self, ctx: &mut Context<Self>) {
        self.weak = Some(ctx.myself_weak());
        // The icon exists before the first answer (`base`).
        self.render();
    }

    async fn handle(&mut self, msg: Msg, ctx: &mut Context<Self>) {
        let weak = ctx.myself_weak();
        if self.lost.swap(false, Ordering::SeqCst) {
            self.request_snapshot(Scope::ALL, &weak);
        }
        match msg {
            Msg::Activate => self.on_activate(&weak),
            Msg::Connection(state) => self.on_connection(&state, &weak),
            Msg::StatesLagged => self.ask_state(&weak),
            Msg::StateAnswer { epoch, result } => {
                if epoch == self.epoch {
                    match result {
                        Ok(state) => self.on_connection(&state, &weak),
                        Err(err) => tracing::debug!(%err, "tray: context state not read"),
                    }
                }
            }
            Msg::Subscribed { epoch, result } => self.on_subscribed(epoch, result, &weak),
            Msg::Frame { epoch, frame } => {
                if epoch == self.epoch && self.stream.as_deref() == Some(frame.stream_id.as_str()) {
                    self.on_frame(frame, &weak);
                }
            }
            Msg::Snapshot {
                epoch,
                snapshot,
                failed,
            } => self.on_snapshot(epoch, snapshot, failed, &weak),
            Msg::Flush => {
                self.flush_scheduled = false;
                if self.menu_open {
                    self.dirty = true;
                } else {
                    self.render();
                }
            }
            Msg::Tick { token } => self.on_tick(token, &weak),
            Msg::MenuTimeout { token } => {
                if token == self.menu_token {
                    self.menu_closed();
                }
            }
            Msg::Retry { epoch } => {
                self.retry_scheduled = false;
                if epoch == self.epoch {
                    // A stream that ended or was refused subscribes again.
                    self.subscribe(&weak);
                    let scope = std::mem::take(&mut self.pending_scope).union(Scope::ALL);
                    self.request_snapshot(scope, &weak);
                }
            }
            Msg::Action(id) => {
                self.menu_closed();
                match TrayAction::parse(&id) {
                    Some(action) => self.on_action(action, &weak),
                    None => tracing::debug!(%id, "tray: menu entry without an action"),
                }
            }
            Msg::Click(click) => self.on_click(click, &weak),
            Msg::Appearance(appearance) => {
                self.appearance = TrayAppearance {
                    reduced_motion: self.appearance.reduced_motion,
                    ..appearance
                };
                self.render();
            }
            Msg::ReducedMotion(reduced) => {
                self.appearance.reduced_motion = reduced;
                self.render();
            }
            Msg::Language => self.render(),
            Msg::AppUpdate(available) => {
                if self.model.app_update != available {
                    self.model.app_update = available;
                    self.changed(&weak);
                }
            }
            Msg::NoticeClicked(kind) => self.view.navigate(kind.route()),
            Msg::NoticeFlush => {
                self.notice_timer = false;
                self.flush_notices(&weak);
            }
            Msg::QuitDone(outcome) => {
                self.quitting = false;
                tracing::info!(?outcome, "tray: quit flow finished");
            }
            Msg::Model(reply) => {
                let _ = reply.send(self.model.clone());
            }
            Msg::Shutdown(reply) => {
                self.close_stream();
                if let Some(task) = self.states_task.take() {
                    task.abort();
                }
                let _ = reply.send(());
            }
        }
    }

    async fn on_stop(&mut self, _ctx: &mut Context<Self>) {
        self.close_stream();
        if let Some(task) = self.states_task.take() {
            task.abort();
        }
    }
}

impl TrayActor {
    fn on_activate(&mut self, weak: &WeakActorRef<Msg>) {
        if self.active {
            return;
        }
        self.active = true;
        // Follow the state first, then ask for it: no change can fall in between.
        let mut states = self.backend.watch_states();
        let context = self.model.context.clone();
        let forward = weak.clone();
        let task = tokio::spawn(async move {
            loop {
                match states.recv().await {
                    Ok(state) if state.context == context => {
                        tell(&forward, Msg::Connection(state));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        tell(&forward, Msg::StatesLagged);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        self.states_task = Some(task.abort_handle());
        self.ask_state(weak);
        self.subscribe(weak);
    }

    fn ask_state(&self, weak: &WeakActorRef<Msg>) {
        let (backend, context, epoch) = (
            Arc::clone(&self.backend),
            self.model.context.clone(),
            self.epoch,
        );
        let timeout = self.options.ask_timeout;
        let weak = weak.clone();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(timeout, backend.state(&context)).await {
                Ok(result) => result,
                Err(_) => Err(ApiError::new("timeout", "context state timed out")),
            };
            tell(&weak, Msg::StateAnswer { epoch, result });
        });
    }

    fn subscribe(&mut self, weak: &WeakActorRef<Msg>) {
        if self.stream.is_some() || self.subscribing || !self.active {
            return;
        }
        self.subscribing = true;
        let sink: Arc<dyn FrameSink> = Arc::new(TraySink {
            actor: weak.clone(),
            backend: Arc::clone(&self.backend),
            epoch: self.epoch,
            lost: Arc::clone(&self.lost),
        });
        let params = json!({ "filter": { "kinds": model::EVENT_KINDS } });
        let call = self
            .backend
            .subscribe_events(&self.model.context, params, sink);
        let (epoch, timeout, weak) = (self.epoch, self.options.call_timeout, weak.clone());
        tokio::spawn(async move {
            let result = match tokio::time::timeout(timeout, call).await {
                Ok(result) => result,
                Err(_) => Err(ApiError::new("timeout", "events.subscribe timed out")),
            };
            tell(&weak, Msg::Subscribed { epoch, result });
        });
    }

    fn on_subscribed(
        &mut self,
        epoch: u64,
        result: Result<SubscribeAnswer, ApiError>,
        weak: &WeakActorRef<Msg>,
    ) {
        if epoch != self.epoch {
            if let Ok(answer) = result {
                self.backend
                    .unsubscribe(&self.model.context, &answer.stream_id);
            }
            return;
        }
        self.subscribing = false;
        match result {
            Ok(answer) => self.stream = Some(answer.stream_id),
            Err(err) => {
                tracing::warn!(%err, "tray: events.subscribe refused");
                self.schedule_retry(weak);
            }
        }
    }

    fn close_stream(&mut self) {
        if let Some(stream) = self.stream.take() {
            self.backend.unsubscribe(&self.model.context, &stream);
        }
    }

    fn on_connection(&mut self, state: &ContextStateDto, weak: &WeakActorRef<Msg>) {
        self.track_notice_cursor(state, weak);
        match self.model.set_connection(state) {
            Effect::None => {}
            Effect::Changed => self.changed(weak),
            Effect::Resync(scope) => {
                self.changed(weak);
                self.request_snapshot(scope, weak);
            }
        }
        if self.model.link == Link::Up {
            self.subscribe(weak);
        }
    }

    /// A new connection's `hello` fixes the toast cursor: events up to its `last_event_seq`
    /// are history, and the resume replay after a reconnection must stay silent. The replay
    /// can reach the actor before the new connection's state does (the client resubscribes
    /// before it publishes the state, and the state takes more hops), so the moment the
    /// connection leaves Connected the engine starts holding events until the new `hello`.
    fn track_notice_cursor(&mut self, state: &ContextStateDto, weak: &WeakActorRef<Msg>) {
        let Some((engine, _)) = self.notices.as_mut() else {
            return;
        };
        if state.context != self.model.context {
            return;
        }
        if state.status != ConnectionStatus::Connected {
            engine.await_cursor();
            self.notice_generation = None;
            return;
        }
        if state.generation == self.notice_generation {
            return;
        }
        if let Some(hello) = &state.hello {
            engine.set_cursor(hello.last_event_seq);
            self.notice_generation = state.generation;
            self.arm_notice_timer(weak);
        }
    }

    /// Hands the engine an event; the toast comes when its window ends.
    fn notice(&mut self, seq: Option<u64>, kind: &str, payload: &Value, weak: &WeakActorRef<Msg>) {
        let Some((engine, _)) = self.notices.as_mut() else {
            return;
        };
        engine.observe(std::time::Instant::now(), seq, kind, payload);
        self.arm_notice_timer(weak);
    }

    /// Wakes the actor when the earliest grouping window ends.
    fn arm_notice_timer(&mut self, weak: &WeakActorRef<Msg>) {
        if self.notice_timer {
            return;
        }
        let Some(deadline) = self
            .notices
            .as_ref()
            .and_then(|(engine, _)| engine.next_deadline())
        else {
            return;
        };
        self.notice_timer = true;
        let weak = weak.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            tell(&weak, Msg::NoticeFlush);
        });
    }

    /// Shows the toasts of the windows that ended and re-arms for the rest.
    fn flush_notices(&mut self, weak: &WeakActorRef<Msg>) {
        if let Some((engine, notices)) = self.notices.as_mut() {
            let lang = i18n::current_language();
            for notice in engine.flush_due(std::time::Instant::now()) {
                notices.notifier.notify(&Toast::from_notice(&notice, lang));
            }
        }
        self.arm_notice_timer(weak);
    }

    fn on_frame(&mut self, frame: StreamFrame, weak: &WeakActorRef<Msg>) {
        let mut resync = Scope::default();
        let mut changed = false;
        for entry in frame.entries {
            match entry {
                StreamEntry::Item { item } => {
                    let event = &item["event"];
                    let Some(kind) = event["kind"].as_str() else {
                        continue;
                    };
                    let payload = event.get("payload").cloned().unwrap_or(Value::Null);
                    self.notice(item["seq"].as_u64(), kind, &payload, weak);
                    if self.snapshot_inflight {
                        self.queued.push((kind.to_owned(), payload));
                        continue;
                    }
                    match self.model.apply_event(kind, &payload) {
                        Effect::None => {}
                        Effect::Changed => changed = true,
                        Effect::Resync(scope) => {
                            changed = true;
                            resync = resync.union(scope);
                        }
                    }
                }
                // Something was lost, or the daemon's sequence restarted: what the tray
                // holds may be stale.
                StreamEntry::Lagged { .. } | StreamEntry::Reset => resync = Scope::ALL,
                StreamEntry::End { error } => {
                    tracing::info!(?error, "tray: event stream ended");
                    self.stream = None;
                    self.schedule_retry(weak);
                }
            }
        }
        if changed {
            self.changed(weak);
        }
        if !resync.is_empty() {
            self.request_snapshot(resync, weak);
        }
    }

    fn request_snapshot(&mut self, scope: Scope, weak: &WeakActorRef<Msg>) {
        if scope.is_empty() {
            return;
        }
        if self.model.link != Link::Up || self.snapshot_inflight {
            self.pending_scope = self.pending_scope.union(scope);
            return;
        }
        let scope = scope.union(std::mem::take(&mut self.pending_scope));
        self.snapshot_inflight = true;
        let mail = scope.services && self.model.mail_visible();
        let fetch = fetch_snapshot(
            Arc::clone(&self.backend),
            self.model.context.clone(),
            scope,
            mail,
            self.options.call_timeout,
        );
        let (epoch, weak) = (self.epoch, weak.clone());
        tokio::spawn(async move {
            let (snapshot, failed) = fetch.await;
            tell(
                &weak,
                Msg::Snapshot {
                    epoch,
                    snapshot,
                    failed,
                },
            );
        });
    }

    fn on_snapshot(
        &mut self,
        epoch: u64,
        snapshot: Snapshot,
        failed: bool,
        weak: &WeakActorRef<Msg>,
    ) {
        if epoch != self.epoch {
            return;
        }
        self.snapshot_inflight = false;
        self.model.apply_snapshot(snapshot);
        // Events that came while the answer was on its way, in order: they may be newer.
        let mut resync = Scope::default();
        for (kind, payload) in std::mem::take(&mut self.queued) {
            if let Effect::Resync(scope) = self.model.apply_event(&kind, &payload) {
                resync = resync.union(scope);
            }
        }
        self.changed(weak);
        if failed {
            self.schedule_retry(weak);
        } else {
            self.retry_failures = 0;
        }
        let next = resync.union(std::mem::take(&mut self.pending_scope));
        self.request_snapshot(next, weak);
    }

    fn schedule_retry(&mut self, weak: &WeakActorRef<Msg>) {
        if self.retry_scheduled {
            return;
        }
        self.retry_scheduled = true;
        // 3 s, 6 s, 12 s... up to a minute while it keeps failing.
        let after = (self.options.retry_after * 2u32.pow(self.retry_failures.min(5)))
            .min(Duration::from_secs(60));
        self.retry_failures = self.retry_failures.saturating_add(1);
        let (epoch, weak) = (self.epoch, weak.clone());
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            tell(&weak, Msg::Retry { epoch });
        });
    }

    /// The model changed: render within the debounce.
    ///
    /// At most one render per [`TrayOptions::debounce`]: the first change after a quiet
    /// period renders at once, later ones are coalesced into one render when the period
    /// ends. While the menu is open the render waits for it to close.
    fn changed(&mut self, weak: &WeakActorRef<Msg>) {
        if self.flush_scheduled {
            return;
        }
        let now = tokio::time::Instant::now();
        let due = self
            .last_render_at
            .map(|at| at + self.options.debounce)
            .filter(|due| *due > now);
        let Some(due) = due else {
            if self.menu_open {
                self.dirty = true;
            } else {
                self.render();
            }
            return;
        };
        self.flush_scheduled = true;
        let weak = weak.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(due).await;
            tell(&weak, Msg::Flush);
        });
    }

    fn icon_key(&self) -> IconKey {
        let state = self.model.state();
        IconKey {
            state,
            frame: if state == TrayState::Starting {
                self.frame
            } else {
                0
            },
            taskbar_light: self.appearance.taskbar_light,
            remote: self.model.remote,
            size: icon_size_for_scale(self.appearance.scale),
        }
    }

    /// Renders now when something the view shows changed.
    fn render(&mut self) {
        let lang = i18n::current_language();
        let mut next = TrayRender {
            revision: self.revision + 1,
            menu: self.model.menu(lang),
            tooltip: self.model.tooltip(lang),
            icon: self.icon_key(),
            dark_menu: self.appearance.dark_menu,
        };
        let same = self.last.as_ref().is_some_and(|last| {
            last.menu == next.menu
                && last.tooltip == next.tooltip
                && last.icon == next.icon
                && last.dark_menu == next.dark_menu
        });
        if !same {
            self.revision += 1;
            next.revision = self.revision;
            self.view.render(&next);
            self.last = Some(next);
            self.last_render_at = Some(tokio::time::Instant::now());
        }
        self.dirty = false;
        self.update_ticking();
    }

    fn update_ticking(&mut self) {
        let animate = self.model.state() == TrayState::Starting && !self.appearance.reduced_motion;
        if animate && !self.ticking {
            self.ticking = true;
            self.tick_token += 1;
            self.schedule_tick();
        } else if !animate && self.ticking {
            self.ticking = false;
            self.tick_token += 1;
            self.frame = 0;
        }
    }

    fn schedule_tick(&self) {
        let Some(weak) = self.weak.clone() else {
            return;
        };
        let (after, token) = (self.options.frame_interval, self.tick_token);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            tell(&weak, Msg::Tick { token });
        });
    }

    fn on_tick(&mut self, token: u64, _weak: &WeakActorRef<Msg>) {
        if token != self.tick_token || !self.ticking {
            return;
        }
        self.frame = (self.frame + 1) % STARTING_FRAMES;
        let key = self.icon_key();
        if let Some(last) = &mut self.last {
            last.icon = key;
        }
        self.view.set_icon(key);
        self.schedule_tick();
    }

    fn on_click(&mut self, click: Click, weak: &WeakActorRef<Msg>) {
        match click {
            Click::Left => {
                self.menu_closed();
                self.view.show_window();
            }
            Click::Middle => {
                self.menu_closed();
                if self.model.terminal_visible() {
                    self.view.open_terminal();
                }
            }
            Click::MenuOpening => {
                self.menu_open = true;
                self.menu_token += 1;
                let (after, token, weak) = (
                    self.options.menu_open_timeout,
                    self.menu_token,
                    weak.clone(),
                );
                tokio::spawn(async move {
                    tokio::time::sleep(after).await;
                    tell(&weak, Msg::MenuTimeout { token });
                });
            }
            Click::MenuClosed => self.menu_closed(),
        }
    }

    fn menu_closed(&mut self) {
        if !self.menu_open {
            return;
        }
        self.menu_open = false;
        self.menu_token += 1;
        if self.dirty {
            self.render();
        }
    }

    fn on_action(&mut self, action: TrayAction, weak: &WeakActorRef<Msg>) {
        if let Some((method, params)) = action.api_call() {
            if self.model.link != Link::Up {
                tracing::info!(method, "tray: action ignored, the daemon is not connected");
                return;
            }
            let args = CallArgs {
                context: self.model.context.clone(),
                method: method.to_owned(),
                params: Some(params),
                // One intent, one fresh request_id (ADR-0009 rule 4).
                request_id: Some(RequestId::new().to_string()),
                timeout_ms: Some(duration_ms(self.options.action_timeout)),
            };
            let call = self.backend.call(args);
            let timeout = self.options.action_timeout;
            tokio::spawn(async move {
                match tokio::time::timeout(timeout, call).await {
                    Ok(Ok(_)) => tracing::debug!(method, "tray: action sent"),
                    Ok(Err(err)) => tracing::warn!(method, %err, "tray: action failed"),
                    Err(_) => tracing::warn!(method, "tray: action timed out"),
                }
            });
            return;
        }
        if let Some(route) = action.route() {
            self.view.navigate(route);
            return;
        }
        match action {
            TrayAction::OpenWindow => self.view.show_window(),
            TrayAction::OpenSite(url) => self.view.open_url(&url),
            TrayAction::Terminal => {
                if self.model.terminal_visible() {
                    self.view.open_terminal();
                }
            }
            TrayAction::SwitchContext(context) => self.switch_context(&context, weak),
            TrayAction::Quit => {
                if self.quitting {
                    return;
                }
                self.quitting = true;
                let (backend, view, context, timeout, weak) = (
                    Arc::clone(&self.backend),
                    Arc::clone(&self.view),
                    self.model.context.clone(),
                    self.options.quit_timeout,
                    weak.clone(),
                );
                tokio::spawn(async move {
                    let outcome =
                        quit_flow(backend.as_ref(), view.as_ref(), &context, timeout).await;
                    tell(&weak, Msg::QuitDone(outcome));
                });
            }
            _ => {}
        }
    }

    fn switch_context(&mut self, context: &str, weak: &WeakActorRef<Msg>) {
        if context == self.model.context || !self.model.contexts.iter().any(|c| c == context) {
            return;
        }
        self.close_stream();
        if let Some(task) = self.states_task.take() {
            task.abort();
        }
        let contexts = self.model.contexts.clone();
        self.epoch += 1;
        self.model = TrayModel::new(context);
        self.model.contexts = contexts;
        self.subscribing = false;
        self.snapshot_inflight = false;
        self.pending_scope = Scope::default();
        self.queued.clear();
        let was_active = self.active;
        self.active = false;
        self.render();
        if was_active {
            self.on_activate(weak);
        }
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// One call of the snapshot: `Ok(None)` when the daemon does not implement it (an empty
/// list, not a failure), `Err` when it failed.
async fn snapshot_call(
    backend: &dyn TrayBackend,
    context: &str,
    method: &str,
    timeout: Duration,
) -> Result<Option<Value>, ApiError> {
    let call = backend.call(CallArgs {
        context: context.to_owned(),
        method: method.to_owned(),
        params: None,
        request_id: None,
        timeout_ms: Some(duration_ms(timeout)),
    });
    match tokio::time::timeout(timeout, call).await {
        Ok(Ok(value)) => Ok(Some(value)),
        Ok(Err(err)) if err.code == "not_implemented" => Ok(None),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(ApiError::new("timeout", format!("{method} timed out"))),
    }
}

/// Fetches the parts of `scope` concurrently. `failed`: some part could not be read (its
/// previous value stays).
fn fetch_snapshot(
    backend: Arc<dyn TrayBackend>,
    context: String,
    scope: Scope,
    mail: bool,
    timeout: Duration,
) -> BoxFuture<'static, (Snapshot, bool)> {
    async move {
        let skip = || async { Ok::<Option<Value>, ApiError>(None) }.boxed();
        let part = |wanted: bool, method: &'static str| {
            if wanted {
                let (backend, context) = (Arc::clone(&backend), context.clone());
                async move { snapshot_call(backend.as_ref(), &context, method, timeout).await }
                    .boxed()
            } else {
                skip()
            }
        };
        let (services, sites, php, mail_status) = futures::join!(
            part(scope.services, "services.status"),
            part(scope.sites, "sites.list"),
            part(scope.php, "php.list"),
            part(mail, "mail.status"),
        );
        let mut failed = false;
        let mut snapshot = Snapshot::default();
        match services {
            Ok(Some(value)) => match serde_json::from_value::<ServiceStatusDto>(value) {
                Ok(dto) => snapshot.services = Some(Snapshot::services_from(&dto)),
                Err(err) => {
                    tracing::warn!(%err, "tray: services.status not understood");
                    failed = true;
                }
            },
            Ok(None) if scope.services => snapshot.services = Some(Vec::new()),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(%err, "tray: services.status failed");
                failed = true;
            }
        }
        match sites {
            Ok(Some(value)) => match serde_json::from_value::<SiteListDto>(value) {
                Ok(dto) => snapshot.sites = Some(Snapshot::sites_from(&dto.sites)),
                Err(err) => {
                    tracing::warn!(%err, "tray: sites.list not understood");
                    failed = true;
                }
            },
            Ok(None) if scope.sites => snapshot.sites = Some(Vec::new()),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(%err, "tray: sites.list failed");
                failed = true;
            }
        }
        match php {
            Ok(Some(value)) => match serde_json::from_value::<PhpVersionListDto>(value) {
                Ok(dto) => snapshot.php = Some(Snapshot::php_from(&dto.versions)),
                Err(err) => {
                    tracing::warn!(%err, "tray: php.list not understood");
                    failed = true;
                }
            },
            Ok(None) if scope.php => snapshot.php = Some(Vec::new()),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(%err, "tray: php.list failed");
                failed = true;
            }
        }
        if let Ok(Some(value)) = mail_status {
            if let Ok(dto) = serde_json::from_value::<MailStatusDto>(value) {
                snapshot.mail_unread = Some(dto.unread);
            }
        }
        (snapshot, failed)
    }
    .boxed()
}

/// "Sair" (docs/07 §7): asks, then either exits at once ("manter"), or sends
/// `daemon.stop {keep_services: false}` with a fresh `request_id` and exits after the
/// answer or `timeout` (5 s), whichever comes first.
pub async fn quit_flow(
    backend: &dyn TrayBackend,
    view: &dyn TrayView,
    context: &str,
    timeout: Duration,
) -> QuitOutcome {
    match view.ask_quit().await {
        QuitChoice::Cancel => QuitOutcome::Cancelled,
        QuitChoice::KeepServices => {
            view.exit();
            QuitOutcome::Exited
        }
        QuitChoice::StopServices => {
            let call = backend.call(CallArgs {
                context: context.to_owned(),
                method: "daemon.stop".to_owned(),
                params: Some(json!({ "keep_services": false })),
                request_id: Some(RequestId::new().to_string()),
                timeout_ms: Some(duration_ms(timeout)),
            });
            let result = match tokio::time::timeout(timeout, call).await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(err)) => Err(err),
                Err(_) => Err(ApiError::new("timeout", "daemon.stop timed out")),
            };
            if let Err(err) = &result {
                tracing::warn!(%err, "tray: daemon.stop did not answer; exiting anyway");
            }
            view.exit();
            QuitOutcome::StoppedAndExited(result)
        }
    }
}
