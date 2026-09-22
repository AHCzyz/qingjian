use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windows::Win32::UI::TextServices::{ITfComposition, ITfContext};

use crate::com::service::SharedClient;

/// `TextService`、编辑会话、组句 sink、轮询定时器之间共享的组句状态（STA 单线程，`Rc` 传递）。
pub(crate) struct Shared {
    /// 当前活动的组句，跨按键存活。
    composition: RefCell<Option<ITfComposition>>,

    /// Server 上次回的帧非空；决定 `OnTestKeyDown` 要不要吃功能键。
    composing: Cell<bool>,

    /// 这一段的输入框状态（私密与否、光标前文）已经报给 Server 了。
    /// 行内模式下靠「组句刚起」判断，`preedit = window` 模式应用里没有组句，得另记一个标记。
    context_reported: Cell<bool>,

    /// 「翻译选中文字」评审进行中：所有键交给 Server 定接受 / 取消，轮询定时器照常拉云端译文。
    translating: Cell<bool>,

    /// 最近一次收键的文档上下文；失焦 / 停用回调不带上下文，落定拼音要用它。
    last_context: RefCell<Option<ITfContext>>,

    /// 组句被应用强行终止过：拼音已成普通文本，但 Server 的缓冲还在，下次说话前先让它清空。
    server_stale: Cell<bool>,

    /// 本线程当前有键盘焦点（`OnSetFocus`）；轮询定时器只在前台时问状态条的切模式请求。
    foreground: Cell<bool>,

    /// 与 `TextService` 共用的引擎客户端；DLL 侧结束组句时要通知 Server 收候选窗口（它无从知晓）。
    client: SharedClient,

    /// 尚未落定的组句更新（提交文本 + 拼音行），按文档上下文分槽排队。
    /// 每个文档上下文至多一个 in-flight 编辑会话：同一上下文的按键结果合并进
    /// 该槽的快照、回调取最新值，杜绝 async 编辑会话乱序落盘（#28 `-6gpt`）；
    /// 跨上下文各排各的，会话只取自己那份，编辑会话不会把别的输入框的拼音
    /// 写进这个文档。
    pending_update: RefCell<Vec<(usize, Option<String>, String)>>,
}

impl Shared {
    pub(crate) fn new(client: SharedClient) -> Rc<Self> {
        Rc::new(Self {
            composition: RefCell::new(None),
            composing: Cell::new(false),
            context_reported: Cell::new(false),
            translating: Cell::new(false),
            last_context: RefCell::new(None),
            server_stale: Cell::new(false),
            foreground: Cell::new(false),
            client,
            pending_update: RefCell::new(Vec::new()),
        })
    }

    pub(crate) fn foreground(&self) -> bool {
        self.foreground.get()
    }

    pub(crate) fn set_foreground(&self, value: bool) {
        self.foreground.set(value);
    }

    pub(crate) fn last_context(&self) -> Option<ITfContext> {
        self.last_context.borrow().clone()
    }

    pub(crate) fn set_last_context(&self, context: Option<ITfContext>) {
        *self.last_context.borrow_mut() = context;
    }

    pub(crate) fn take_server_stale(&self) -> bool {
        self.server_stale.replace(false)
    }

    pub(crate) fn composing(&self) -> bool {
        self.composing.get()
    }

    pub(crate) fn set_composing(&self, value: bool) {
        self.composing.set(value);
        // 组句结束：下一段重新报输入框状态。
        if !value {
            self.context_reported.set(false);
        }
    }

    pub(crate) fn context_reported(&self) -> bool {
        self.context_reported.get()
    }

    pub(crate) fn set_context_reported(&self, value: bool) {
        self.context_reported.set(value);
    }

    pub(crate) fn translating(&self) -> bool {
        self.translating.get()
    }

    pub(crate) fn set_translating(&self, value: bool) {
        self.translating.set(value);
    }

    /// 通知 Server 收起候选窗口。引擎正被别处借着或没连上时静默跳过。
    pub(crate) fn hide_candidates(&self) {
        if let Ok(mut guard) = self.client.try_borrow_mut()
            && let Some(client) = guard.as_mut()
        {
            let _ = client.hide_candidates();
        }
    }

    pub(crate) fn has_composition(&self) -> bool {
        self.composition.borrow().is_some()
    }
}

/// ITfContext 的身份令牌：同一文档上下文（同一 COM 对象）恒等，只拿指针数值做身份比较
/// （沿用 TSF 每次下发同一上下文指针的约定），不持有引用计数。
pub(crate) fn context_token(context: &ITfContext) -> usize {
    <ITfContext as windows::core::Interface>::as_raw(context) as usize
}

impl Shared {
    /// 入队一次组句更新。同一上下文已有未落定的快照时合并：提交文本拼接（`gpt-` + `6` → `gpt-6`）、
    /// 提交 + 新组句合成（先提交再重新显示 preedit）。返回是否要新发一个编辑会话请求
    /// （该上下文没有在飞的快照时才需要；每个文档上下文至多一个 in-flight 会话，
    /// 跨上下文各排各的队，编辑会话只取自己那份，杜绝 A 的会话把 B 的拼音写进 A 的文档）。
    pub(crate) fn enqueue_update(
        &self,
        context: usize,
        commit: Option<String>,
        preedit: String,
    ) -> bool {
        let mut pending = self.pending_update.borrow_mut();
        let fresh = !pending.iter().any(|(token, _, _)| *token == context);
        if let Some((_, pending_commit, pending_preedit)) =
            pending.iter_mut().find(|(token, _, _)| *token == context)
        {
            *pending_commit = match (pending_commit.take(), commit) {
                (Some(a), Some(b)) => Some(a + &b),
                (a, b) => a.or(b),
            };
            *pending_preedit = preedit;
        } else {
            pending.push((context, commit, preedit));
        }
        fresh
    }

    /// 编辑会话回调取走本上下文的快照；别的上下文（或已取走）返回 `None`，队列里的留着。
    pub(crate) fn take_pending_update(&self, context: usize) -> Option<(Option<String>, String)> {
        let mut pending = self.pending_update.borrow_mut();
        let position = pending.iter().position(|(token, _, _)| *token == context)?;
        let (_, commit, preedit) = pending.remove(position);
        Some((commit, preedit))
    }

    /// 编辑会话请求失败时清掉该上下文的快照，避免队列永远卡住；别的上下文不受影响。
    pub(crate) fn clear_pending_update(&self, context: usize) {
        self.pending_update
            .borrow_mut()
            .retain(|(token, _, _)| *token != context);
    }

    /// 还有未落定的组句更新（编辑会话在飞或有快照排队）。
    pub(crate) fn has_pending_update(&self) -> bool {
        !self.pending_update.borrow().is_empty()
    }

    /// clone 出来再用，别把 `borrow()` 挂在 match 上（分支里再 `borrow_mut` 会 panic）。
    pub(super) fn composition(&self) -> Option<ITfComposition> {
        self.composition.borrow().clone()
    }

    pub(super) fn set_composition(&self, composition: Option<ITfComposition>) {
        *self.composition.borrow_mut() = composition;
    }

    pub(super) fn take_composition(&self) -> Option<ITfComposition> {
        self.composition.borrow_mut().take()
    }

    /// 组句结束（应用终止组句 / 断线 / 失焦上屏）：不再当作在组句，并让 Server 收候选窗口。
    pub(crate) fn end_composing(&self) {
        self.composing.set(false);
        self.translating.set(false);
        self.hide_candidates();
    }

    /// 清掉一切本地组句状态，不碰文档。
    pub(crate) fn reset(&self) {
        self.pending_update.borrow_mut().clear();
        self.set_composition(None);
        self.set_last_context(None);
        self.end_composing();
    }

    /// 组句被应用强行终止：本地清掉，并记下 Server 的缓冲还没清。
    pub(super) fn terminated(&self) {
        self.reset();
        self.server_stale.set(true);
    }
}

#[cfg(test)]
mod tests {
    use super::Shared;
    use crate::com::service::SharedClient;

    /// 两个不同「文档上下文」的身份令牌（测试里用任意不相等的数值）。
    const CTX_A: usize = 0xAAA;
    const CTX_B: usize = 0xBBB;

    fn shared() -> std::rc::Rc<Shared> {
        Shared::new(SharedClient::default())
    }

    #[test]
    fn merge_appends_consecutive_commits() {
        let s = shared();
        // 组句提交 `gpt-` 还没落定，又来一个提交 `6`：拼接成一个快照，乱序落盘不可能发生。
        assert!(s.enqueue_update(CTX_A, Some("gpt-".to_owned()), String::new()));
        assert!(!s.enqueue_update(CTX_A, Some("6".to_owned()), String::new()));
        assert_eq!(
            s.take_pending_update(CTX_A),
            Some((Some("gpt-6".to_owned()), String::new()))
        );
    }

    #[test]
    fn merge_replaces_pending_preedit_with_new_commit() {
        let s = shared();
        // 组句显示 `g'p't`（纯 preedit 快照未落）又来一个提交：preedit 让位，提交优先。
        assert!(s.enqueue_update(CTX_A, None, "g'p't".to_owned()));
        assert!(!s.enqueue_update(CTX_A, Some("gpt-".to_owned()), String::new()));
        assert_eq!(
            s.take_pending_update(CTX_A),
            Some((Some("gpt-".to_owned()), String::new()))
        );
    }

    #[test]
    fn merge_combines_pending_commit_with_new_preedit() {
        let s = shared();
        // 提交 `gpt-` 未落定，新键开始新组句：合成「先提交再显示新 preedit」。
        assert!(s.enqueue_update(CTX_A, Some("gpt-".to_owned()), String::new()));
        assert!(!s.enqueue_update(CTX_A, None, "q".to_owned()));
        assert_eq!(
            s.take_pending_update(CTX_A),
            Some((Some("gpt-".to_owned()), "q".to_owned()))
        );
    }

    #[test]
    fn fresh_queue_requests_new_session_after_take() {
        let s = shared();
        assert!(s.enqueue_update(CTX_A, None, "a".to_owned()));
        assert_eq!(s.take_pending_update(CTX_A), Some((None, "a".to_owned())));
        assert!(
            s.enqueue_update(CTX_A, Some("b".to_owned()), String::new()),
            "队列清空后应重新发起会话"
        );
        assert_eq!(
            s.take_pending_update(CTX_A),
            Some((Some("b".to_owned()), String::new()))
        );
    }

    #[test]
    fn contexts_queue_independently_and_take_their_own() {
        let s = shared();
        // A 的会话在飞（快照未落）时 B 的键先到：各排各的队，都要新开会话。
        assert!(s.enqueue_update(CTX_A, None, "g'p't".to_owned()));
        assert!(s.enqueue_update(CTX_B, Some("b".to_owned()), String::new()));
        // B 的会话先跑：只取 B 的快照，A 的留着。
        assert_eq!(
            s.take_pending_update(CTX_B),
            Some((Some("b".to_owned()), String::new()))
        );
        assert_eq!(
            s.take_pending_update(CTX_A),
            Some((None, "g'p't".to_owned()))
        );
        assert_eq!(s.take_pending_update(CTX_A), None, "取走后队列为空");
    }

    #[test]
    fn same_context_merges_other_context_is_untouched() {
        let s = shared();
        assert!(s.enqueue_update(CTX_A, None, "a".to_owned()));
        assert!(s.enqueue_update(CTX_B, None, "x".to_owned()));
        // A 又来了一个键：合并进 A 的槽，B 的槽原样。
        assert!(!s.enqueue_update(CTX_A, Some("1".to_owned()), String::new()));
        assert!(s.has_pending_update());
        assert_eq!(
            s.take_pending_update(CTX_A),
            Some((Some("1".to_owned()), String::new()))
        );
        assert_eq!(s.take_pending_update(CTX_B), Some((None, "x".to_owned())));
        assert!(!s.has_pending_update());
    }

    #[test]
    fn clear_only_drops_the_failed_context() {
        let s = shared();
        assert!(s.enqueue_update(CTX_A, None, "a".to_owned()));
        assert!(s.enqueue_update(CTX_B, None, "x".to_owned()));
        // A 的会话请求失败：只清 A 的槽，B 的照常落定。
        s.clear_pending_update(CTX_A);
        assert!(s.has_pending_update(), "B 的槽应还在");
        assert_eq!(s.take_pending_update(CTX_B), Some((None, "x".to_owned())));
        assert!(!s.has_pending_update());
    }
}
