//! 按键怎么作用到 Engine / 高亮上。分流规则与 macOS 壳的 `handle_text` / `handle_command` 对齐。

use qingjian_core::{QUESTION_PREFIX, shortcut};
use qingjian_platform::protocol::KeyEvent;

use super::{Effect, codes, with_prefix};
use crate::dispatch::Router;

impl Router {
    /// 功能键靠键码，其余靠字符。组句中修饰键 + 数字是快捷键；带 Ctrl / Alt / Win 而没配到快捷键的键归应用。
    /// 表达式模式里 Shift + 数字打的是 `^ * ( )`，不当快捷键。
    pub(crate) fn apply_key(&mut self, event: &KeyEvent) -> Effect {
        if self.composing()
            && !self.engine.expression_mode()
            && let Some(digit) = codes::digit_key(event.virtual_key)
            && let Some(effect) = self.apply_digit_shortcut(digit, event.modifiers.chord())
        {
            return effect;
        }
        if event.modifiers.has_command_key() {
            return Effect::Passthrough;
        }
        let Some(c) = event.character.filter(|c| !c.is_control()) else {
            return self.apply_function_key(event);
        };
        // 终端 / 控制台类宿主（conhost 及同类）：逐键直通、绝不挂组句。
        // 这类宿主的键盘层在「输入法开着组句」时会吞掉数字 / 符号键（键根本没送到输入法，
        // #28 的终端场景）；不挂组句就没有可吞的打开状态——和搜狗在终端里表现一致。
        if self.console_host() {
            self.engine.note_passthrough(c);
            return Effect::Changed(Some(c.to_string()));
        }
        // Caps 亮着无论中英模式都直接出大写英文；英文候选只在持久英文模式、Caps 灭、应用允许时给。
        let caps = event.modifiers.caps;
        let english = caps || event.modifiers.english_mode;
        let english_candidates = event.modifiers.english_mode
            && !caps
            && self.config.english_candidates_in(self.focused_app());
        // 缓冲区为空时敲 `?` 先进问字模式（配置 `[shortcut] question_mark`，缺省关），中英文模式都行：
        // 后面跟字母就是在问字，跟别的键就还原成问号。
        if !self.composing() && c == QUESTION_PREFIX && self.engine.takes_question_mark() {
            self.engine.set_english_mode(false);
            self.engine.push(c);
            return Effect::Changed(None);
        }
        // 双拼下 Shift+V / Shift+U 进表达式 / 问字模式（全拼下的 v / u 被音节占了）。
        if !self.composing() && !english && self.engine.takes_mode_letter(c) {
            self.engine.set_english_mode(false);
            self.engine.push(c);
            return Effect::Changed(None);
        }
        let question = self.composing() && self.engine.question_mode();
        // 英文模式下问字：Caps 让字母以大写送来，按小写收进问题。
        let c = if question && english && c.is_ascii_uppercase() {
            c.to_ascii_lowercase()
        } else {
            c
        };
        // 只有一个 `?` 时敲了字母以外的键：还原成问号上屏；空格只是「把这个 ? 上屏」，其他键按没在组句重新分派。
        if question && !c.is_ascii_lowercase() && self.engine.bare_question() {
            let mark = self.restore_bare_question(english);
            if c == ' ' {
                return Effect::Changed(Some(mark));
            }
            return with_prefix(Some(mark), self.apply_key(event), c);
        }
        // 英文组词中候选被关掉（Caps 亮 / 切应用）：敲过的字母先原样上屏。
        let flushed = (self.composing() && !english_candidates && self.engine.english_mode())
            .then(|| self.engine.take_raw());
        self.engine
            .set_english_mode(english_candidates && !question);
        let effect = if english && !question {
            self.apply_english(c, english_candidates, event)
        } else {
            self.apply_chinese(c, event)
        };
        with_prefix(flushed, effect, c)
    }

    /// 缓冲区里只有一个 `?`：清掉，还原成问号（按当前模式的全角设置转）。
    fn restore_bare_question(&mut self, english: bool) -> String {
        self.engine.clear();
        if self.full_width_for(english)
            && let Some(mark) = self.engine.punctuate(QUESTION_PREFIX)
        {
            return mark.to_owned();
        }
        self.engine.note_passthrough(QUESTION_PREFIX);
        QUESTION_PREFIX.to_string()
    }

    /// 退格 / Esc / 回车 / Tab / 方向键；没在组句时都交还应用。
    fn apply_function_key(&mut self, event: &KeyEvent) -> Effect {
        if !self.composing() {
            // 回车交给应用：文本流里是一个段落边界（macOS 壳同样记）
            if event.virtual_key == codes::RETURN {
                self.engine.note_passthrough('\n');
            }
            return Effect::Passthrough;
        }
        // 只有一个 `?` 时按了回车：回车就是「把这个 ? 上屏」，吞掉，否则聊天框会连消息一起发出去；
        // 退格 / Esc 照常删掉它。其他功能键 macOS 壳还原后交给应用，Windows 放行同步、上屏异步，
        // 先动光标再插问号会插错位置，所以还原后一并吞掉。
        if self.engine.bare_question() && !matches!(event.virtual_key, codes::BACK | codes::ESCAPE)
        {
            let english = event.modifiers.caps || event.modifiers.english_mode;
            return Effect::Changed(Some(self.restore_bare_question(english)));
        }
        match event.virtual_key {
            codes::BACK => {
                self.engine.backspace();
                Effect::Changed(None)
            }
            codes::ESCAPE => {
                // 辅码态里 Esc 只清码段、拼音留着（与 Core 的 clear_aux 语义一致）
                if self.engine.in_aux() {
                    self.engine.clear_aux();
                } else {
                    self.engine.clear();
                }
                Effect::Changed(None)
            }
            codes::RETURN => {
                if self.engine.is_zhuyin_mode() {
                    if event.modifiers.shift {
                        Effect::Changed(Some(self.engine.take_raw()))
                    } else {
                        Effect::Changed(Some(self.commit_highlighted()))
                    }
                } else {
                    Effect::Changed(Some(self.engine.take_raw()))
                }
            }
            codes::TAB if event.modifiers.shift => {
                self.page(-1);
                Effect::Navigated
            }
            codes::TAB if self.engine.english_mode() => {
                Effect::Changed(Some(self.commit_highlighted()))
            }
            // 中文模式 Tab：有整句补全就接受，否则下一页。
            codes::TAB => match self.sentence.take() {
                Some(sentence) => Effect::Changed(Some(self.engine.accept_prediction(&sentence))),
                None => {
                    self.page(1);
                    Effect::Navigated
                }
            },
            codes::DOWN => {
                self.move_highlight(1);
                Effect::Navigated
            }
            codes::UP => {
                self.move_highlight(-1);
                Effect::Navigated
            }
            codes::NEXT => {
                self.page(1);
                Effect::Navigated
            }
            codes::PRIOR => {
                self.page(-1);
                Effect::Navigated
            }
            codes::LEFT => {
                self.engine.move_cursor_left();
                Effect::Changed(None)
            }
            codes::RIGHT => {
                self.engine.move_cursor_right();
                Effect::Changed(None)
            }
            codes::HOME => {
                self.engine.move_cursor_home();
                Effect::Changed(None)
            }
            codes::END => {
                self.engine.move_cursor_end();
                Effect::Changed(None)
            }
            _ => Effect::Passthrough,
        }
    }

    /// 中文模式：字母进拼音。缺省 Shift 大写是临时打英文——组句中先把拼音原样上屏、字母交给应用；
    /// 配 `[general] shift_letter = "compose"` 时大写也收进缓冲区（Core 按小写匹配、原样上屏时还原大小写）。
    /// 没在组句时的其他字符走全角标点（与 macOS 壳一致，组句中的标点仍进英文直输段）。
    fn apply_chinese(&mut self, c: char, event: &KeyEvent) -> Effect {
        // 注音模式下数字与 `- ; , . /` 就是键盘上的音节键，跟着进缓冲区。
        let is_zhuyin_key = self.engine.is_zhuyin_mode()
            && (c.is_ascii_digit() || matches!(c, '-' | ';' | ',' | '.' | '/'));
        if c.is_ascii_lowercase()
            || is_zhuyin_key
            || (c.is_ascii_uppercase() && self.config.shift_letter_compose)
        {
            // 辅码态：字母进码段（逐键即筛），不进拼音缓冲区
            if c.is_ascii_lowercase() && self.engine.push_aux_code(c) {
                return Effect::Changed(None);
            }
            // 大写（shift_letter_compose）落在辅码态：先清码段回拼音态（与 Esc 同语义）再收进缓冲区；
            // 码段不清会挂在已变化的缓冲区上继续筛
            if c.is_ascii_uppercase() && self.engine.in_aux() {
                self.engine.clear_aux();
            }
            self.engine.push(c);
            return Effect::Changed(None);
        }
        if c.is_ascii_uppercase() {
            let raw = self.composing().then(|| self.engine.take_raw());
            self.engine.note_passthrough(c);
            return with_prefix(raw, Effect::Passthrough, c);
        }
        if !self.composing() {
            return self.apply_punctuation(c, event);
        }
        self.apply_printable(c, event)
    }

    /// 当前模式开着全角就让 Core 转（数字后的 `.` 与小键盘的键保持半角）；转不了的原样交给应用并告知 Core。
    /// （放行时若 DLL 本地还挂着未落定的组句，DLL 会吃下并走提交队列，见 key_sink 放行护栏。）
    fn apply_punctuation(&mut self, c: char, event: &KeyEvent) -> Effect {
        let english = event.modifiers.caps || event.modifiers.english_mode;
        if !codes::is_keypad(event.virtual_key)
            && self.full_width_for(english)
            && let Some(text) = self.engine.punctuate(c)
        {
            return Effect::Changed(Some(text.to_owned()));
        }
        self.engine.note_passthrough(c);
        Effect::Passthrough
    }

    /// 英文模式。开着候选：字母进缓冲区，选词与中文模式一样（空格选高亮、数字选当前页第 N 个、翻页键翻页），
    /// 词上屏后空格照样交给应用；数字对应的格子没有候选（词表没有的词、候选不足 N 个）时是标识符的一部分（`foo1`）。
    /// 回车 / 标点先把字母原样上屏。关着候选：字母由我们插入（大小写按 Shift）。
    /// 其他键按英文模式那份全角设置转，转不了的交给应用。
    fn apply_english(&mut self, c: char, candidates: bool, event: &KeyEvent) -> Effect {
        let composing = self.composing();
        if !candidates {
            let raw = composing.then(|| self.engine.take_raw());
            let effect = if c.is_ascii_alphabetic() {
                self.engine.note_passthrough(c);
                Effect::Changed(Some(c.to_string()))
            } else {
                self.apply_punctuation(c, event)
            };
            return with_prefix(raw, effect, c);
        }
        if composing
            && let Some(digit) = codes::digit(event)
            && let Some(index) = self.slot_index(digit)
        {
            return Effect::Changed(self.commit_index(index));
        }
        if c.is_ascii_alphabetic()
            || (composing && (c.is_ascii_digit() || matches!(c, '_' | '\'' | '-')))
        {
            self.engine.push(c);
            return Effect::Changed(None);
        }
        if composing && let Some(step) = codes::page_key(event, self.config.page_keys) {
            self.page(step);
            return Effect::Navigated;
        }
        let committed = composing.then(|| {
            if c == ' ' {
                self.commit_highlighted()
            } else {
                self.engine.take_raw()
            }
        });
        let effect = self.apply_punctuation(c, event);
        with_prefix(committed, effect, c)
    }

    /// 组句中的可打印键：数字选当前页第 N 个（没有这一格就进直输段），翻页键翻页，空格上屏高亮，其余进英文直输段；已在直输段里就一律追加。
    /// 表达式模式（`v1+2`）里数字和运算符进算式；问字模式敲的还可能是码点（`u4e00`、`u+1f600`），数字与 `+` 进缓冲区；
    /// 微软 / 搜狗双拼的 `;` 是 ing 键，末尾有落单声母时进缓冲区。
    fn apply_printable(&mut self, c: char, event: &KeyEvent) -> Effect {
        let expression = self.engine.expression_mode();
        if (expression && shortcut::is_expression_char(c))
            || (self.engine.unicode_entry() && (c.is_ascii_digit() || c == '+'))
            || (c == ';' && self.engine.takes_semicolon())
        {
            self.engine.push(c);
            return Effect::Changed(None);
        }
        // 英文直输段（缓冲区里已有 `-` 这类字符）：可见字符一律追加，数字与翻页键也不再选词 / 翻页；
        // 空格整段原样上屏，空格本身也要在（`hello, world`）。
        if self.engine.raw_mode() {
            if c == ' ' {
                let committed = self.commit_highlighted();
                self.engine.note_passthrough(c);
                return with_prefix(Some(committed), Effect::Passthrough, c);
            }
            if c.is_ascii_graphic() {
                self.engine.push(c);
                return Effect::Changed(None);
            }
        }
        // 辅码触发键：拼音打完整了、这个键也没被键盘方案吃掉 → 进辅码态（触发键不进缓冲区）
        if self.engine.aux_trigger(c) {
            self.engine.enter_aux();
            return Effect::Changed(None);
        }
        // 已经在辅码态里再敲触发键是幂等的，既不上屏候选也不当标点
        if self.engine.in_aux() && c == self.config.aux_code_key {
            return Effect::Changed(None);
        }
        // 码段筛空（例如码敲错一个字母）：空格 / 标点 / 数字都不上屏原始拼音，吞掉停在辅码态等退格
        if self.engine.in_aux() && !self.engine.aux_code().is_empty() && self.candidate_count() == 0
        {
            return Effect::Changed(None);
        }
        // 主键区数字：有候选就选（正常中文流程，无条件——v4 用末音节/混输做守卫污染了
        // `w`+6 这类选词）；没有这一格（`gpt9` 只有 9 个以下候选）数字当内容进缓冲。
        // 右侧小键盘数字不参与选词：组句里一律直输进缓冲（用户契约，与 `gpt6` 一致，
        // 小键盘的键永不选候选、也不走「候选 + 数字」的提交路径）。
        if let Some(digit) = codes::digit(event)
            && !codes::is_keypad(event.virtual_key)
            && (!self.engine.is_zhuyin_mode() || self.navigated)
        {
            if let Some(index) = self.slot_index(digit) {
                return Effect::Changed(self.commit_index(index));
            }
            // 问字模式里数字不是问题的一部分：没有这一格就不算
            if self.engine.question_mode() {
                return Effect::Changed(None);
            }
            // 这一页没有第 N 格：数字是内容（`gpt9`、`wenti6`），进直输段原样呈现
            self.engine.push(c);
            return Effect::Changed(None);
        }
        // 小键盘键（数字与 `+ - * / .`）：上面已排除选词，到这里的都进缓冲直输，
        // 无论音节是否打完整 —— 候选窗口 raw 呈现、空格 / 回车整体上屏（保序）。
        if codes::is_keypad(event.virtual_key) {
            self.engine.push(c);
            return Effect::Changed(None);
        }
        if let Some(step) = codes::page_key(event, self.config.page_keys) {
            self.page(step);
            return Effect::Navigated;
        }
        if c == ' ' {
            if self.engine.zhuyin_needs_tone() {
                self.engine.push(c);
                return Effect::Changed(None);
            }
            return Effect::Changed(Some(self.commit_highlighted()));
        }
        // 表达式 / 问字模式下的其他字符不进缓冲区（与 macOS 壳一致），辅码态里敲标点同理（码段随之清空）：
        // 都是先把高亮候选上屏，再按没在组句处理这个键、标点按组句外语义转全角
        if (c != '\'' && (expression || self.engine.question_mode())) || self.engine.in_aux() {
            let committed = self.commit_highlighted();
            let effect = self.apply_punctuation(c, event);
            return with_prefix(Some(committed), effect, c);
        }
        // 混输 / 未完成（`gpt6`、`deepseek-`、`gpt,`）：数字与符号进缓冲的英文直输段，
        // 候选窗以原样文本（Raw 兜底）呈现，空格 / 回车确认整体上屏（#28，用户要求的候选呈现）。
        // 完整纯拼音（`wenti,`）：候选「问题」先上屏、标点再跟上（`问题，`），一次提交保序。
        if c != '\'' && (self.mixed_buffer() || !self.trailing_syllable_complete()) {
            self.engine.push(c);
            return Effect::Changed(None);
        }
        if c != '\'' {
            let committed = self.commit_highlighted();
            let effect = self.apply_punctuation(c, event);
            return with_prefix(Some(committed), effect, c);
        }
        self.engine.push(c);
        Effect::Changed(None)
    }

    /// 数字键在当前页对应的格子下标；这一页没有这一格（`gpt6` 只有三个候选）返回 `None`，数字当内容进缓冲区。
    /// 云端词还没到的占位格算有：按了不算，免得结果一到就选错。
    fn slot_index(&self, digit: usize) -> Option<usize> {
        let page_size = self.config.page_size;
        let index = self.highlight / page_size * page_size + digit - 1;
        (digit <= page_size && index < self.candidate_count()).then_some(index)
    }

    /// 上屏高亮候选；没有候选时缓冲原样上屏。
    fn commit_highlighted(&mut self) -> String {
        match self.commit_index(self.highlight) {
            Some(text) => text,
            None => self.engine.take_raw(),
        }
    }

    fn composing(&self) -> bool {
        !self.engine.composition().is_empty()
    }

    /// 缓冲里已混入非拼音字符（数字 / 符号）：后续字符一律进缓冲继续混输。
    /// （大写字母不会进缓冲，`?` 问字前缀不在可打印键路径上。）
    fn mixed_buffer(&self) -> bool {
        self.engine
            .composition()
            .text()
            .bytes()
            .any(|b| !b.is_ascii_lowercase() && b != b'\'')
    }

    /// 缓冲末尾音节是否已打完（`wenti`、`han` 完整；`gpt`、`zho` 的末尾仍是声母 / 前缀）。
    /// 末音节未打完时数字不当选词键、符号按原样上屏：用户多半在打英文词或混合串
    /// （`gpt6`、`deepseek-`，#28）。切分失败（双拼方案等 route 不认识的输入）退回旧行为。
    fn trailing_syllable_complete(&self) -> bool {
        let text = self.engine.composition().text();
        if text.is_empty() {
            return true;
        }
        match qingjian_core::parser::segment(text) {
            Ok(segmentations) => segmentations.iter().any(|s| !s.last_is_partial()),
            Err(_) => true,
        }
    }

    /// 终端 / 控制台类宿主名单：这些宿主的键盘层在「输入法开着组句」时吞数字 / 符号键
    /// （键根本送不到输入法，见 #28 终端场景），因此对它们逐键直通、绝不挂组句，
    /// 与搜狗在终端里的表现一致。conhost 是系统控制台（cmd / powershell）；
    /// 名单按「同样会吞键的控制台类宿主」继续追加。其余终端（如 Windows
    /// Terminal，原生渲染组句）不在名单里，照常中文组句。
    fn console_host(&self) -> bool {
        self.focused_app()
            .is_some_and(|app| app.to_ascii_lowercase().as_str() == "conhost.exe")
    }
}
