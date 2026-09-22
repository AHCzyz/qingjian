//! 混合数字 / 符号输入的顺序回归测试（#28 本地修复验证）。
//!
//! 覆盖两类契约：
//! 1. 中文组句中数字与符号按源输入顺序落定——小键盘键（数字 / 运算符）与
//!    无此格的主键数字一律进缓冲直输（无论音节是否打完整），候选窗 raw 呈现、
//!    空格 / 回车整体上屏，空格一并保留（`gpt6`、`deepseek-`、`wenti5` → `gpt6 ` 等）；
//!    末音节未打完时符号同样进缓冲（`gpt,`、`deepseek-`）；
//! 2. 末音节完整的正常拼音照旧工作：主键数字选候选（`han5` 出汉字、`gpt6` 选词）、
//!    中文标点前出候选（`wenti,` → `问题，`）。

use super::super::Router;
use super::super::config::RouterConfig;
use qingjian_core::Engine;
use qingjian_dictionary::Dictionary;
use qingjian_platform::protocol::{
    ClientMessage, KeyEvent, KeyModifiers, KeyOutcome, PROTOCOL_VERSION, ServerMessage, SessionId,
};
use qingjian_platform::{AppsConfig, KeyCombo};

/// 带足量候选词的小词表：`g`/`p`/`t` 各音节有 6+ 词（旧逻辑会真去选第 N 候选），
/// `han` 有 8 词，`问题`/`下载` 各一条。
const DICT: &str = "\
个\tge\t9000
哥\tge\t8000
给\tgei\t7000
高\tgao\t6000
敢\tgan\t5500
干\tgan\t5000
刚\tgang\t4500
该\tgai\t4000
感\tgan\t3500
顾\tgu\t3000
怕\tpa\t6000
他\tta\t5000
喊\than\t4000
汉\than\t3800
含\than\t3600
寒\than\t3400
汗\than\t3200
旱\than\t3000
憾\than\t2800
翰\than\t2600
问题\twen ti\t3000
下载\txia zai\t2000
";

fn router(dict: &str) -> Router {
    let engine = Engine::new(Dictionary::parse(dict).expect("词表应为合法 TSV"));
    let config = RouterConfig {
        page_size: 9,
        cloud_slots: 0,
        page_keys: ('[', ']'),
        english_candidates: true,
        full_width: true,
        english_full_width: false,
        apps: AppsConfig::default(),
        translation_keys: (KeyModifiers::default(), KeyModifiers::default()),
        delete_keys: KeyModifiers::default(),
        translate_selection: KeyCombo::TRANSLATE_DEFAULT,
        status_enabled: false,
        status_pos: None,
        ..RouterConfig::default()
    };
    Router::new(engine, config)
}

/// 送一个字符键（走完整消息分发，含组句帧重建），返回本次要上屏的文本。
fn key(router: &mut Router, vk: u32, ch: char) -> Option<String> {
    key_at(router, SessionId(1), vk, ch)
}

fn key_at(router: &mut Router, session: SessionId, vk: u32, ch: char) -> Option<String> {
    let event = KeyEvent::new(vk, Some(ch), KeyModifiers::default());
    match router.handle(ClientMessage::Key { session, event }) {
        Some(ServerMessage::KeyResult { commit, .. }) => commit,
        other => panic!("unexpected response: {other:?}"),
    }
}

fn outcome_at(router: &mut Router, session: SessionId, vk: u32, ch: char) -> KeyOutcome {
    let event = KeyEvent::new(vk, Some(ch), KeyModifiers::default());
    match router.handle(ClientMessage::Key { session, event }) {
        Some(ServerMessage::KeyResult { outcome, .. }) => outcome,
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn mixed_word_keeps_order_digit() {
    let mut router = router(DICT);
    for c in "gpt".chars() {
        assert_eq!(key(&mut router, c as u32, c), None, "拼音字母不应上屏");
    }
    // 主键区 6：有候选就选候选（用户契约，v5）。
    let selected = key(&mut router, b'6' as u32, '6').expect("主键区数字应选候选");
    assert_ne!(selected, "gpt6");
    assert!(!selected.is_ascii());
}

#[test]
fn numpad_digit_joins_mixed_candidate() {
    // `gpt` + 小键盘 6：小键盘数字走混输（进直输段呈现候选），空格整体上屏 —— 用户场景。
    let mut router = router(DICT);
    for c in "gpt".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    assert_eq!(key(&mut router, 0x66, '6'), None);
    assert_eq!(key(&mut router, 0x20, ' '), Some("gpt6 ".to_owned()));
}

#[test]
fn numpad_after_complete_syllable_joins_the_buffer() {
    // `han` + 小键盘 6：小键盘键在组句里一律直输，不选候选也不「候选 + 数字」提交
    // （与 `gpt6` 一致）：候选窗 raw 呈现 `han6`，空格整体上屏 `han6 `。
    let mut router = router(DICT);
    for c in "han".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    assert_eq!(key(&mut router, 0x66, '6'), None);
    assert_eq!(key(&mut router, 0x20, ' '), Some("han6 ".to_owned()));
}

#[test]
fn numpad_digit_never_selects_on_complete_syllable() {
    // `wenti`（3 个候选） + 小键盘 5：即使有第 5 格之外的候选也不选，进缓冲直输。
    let mut router = router(DICT);
    for c in "wenti".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    assert_eq!(key(&mut router, 0x65, '5'), None);
    assert_eq!(key(&mut router, 0x20, ' '), Some("wenti5 ".to_owned()));
}

#[test]
fn main_digit_without_a_slot_joins_the_buffer() {
    // `wenti`（3 个候选）+ 主键 6：没有这一格就不选，数字当内容进缓冲（`wenti6 `）。
    let mut router = router(DICT);
    for c in "wenti".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    assert_eq!(key(&mut router, b'6' as u32, '6'), None);
    assert_eq!(key(&mut router, 0x20, ' '), Some("wenti6 ".to_owned()));
}

#[test]
fn mixed_word_keeps_order_symbol() {
    let mut router = router(DICT);
    for c in "deepseek".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    // 末音节 k 未打完：符号进缓冲呈现候选，空格确认上屏
    assert_eq!(key(&mut router, 0xBD, '-'), None);
    assert_eq!(key(&mut router, 0x20, ' '), Some("deepseek- ".to_owned()));
}

#[test]
fn mixed_word_no_dictionary_keeps_order() {
    let mut router = router("");
    for c in "gpt".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    // 无候选时主键区数字也走混输（进缓冲），空格上屏
    assert_eq!(key(&mut router, b'6' as u32, '6'), None);
    assert_eq!(key(&mut router, 0x20, ' '), Some("gpt6 ".to_owned()));
}

#[test]
fn complete_syllable_digit_still_selects() {
    let mut router = router(DICT);
    for c in "han".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    // `han` 末音节完整：5 仍是选第 5 个候选，不能原样上屏成 `han5`
    let committed = key(&mut router, b'5' as u32, '5').expect("应选中一个候选");
    assert_ne!(committed, "han5");
    assert!(!committed.is_ascii());
}

#[test]
fn complete_syllable_comma_commits_candidate_then_punct() {
    let mut router = router(DICT);
    for c in "wenti".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    // 完整纯拼音遇标点：候选「问题」先上屏、标点随后，一次提交保序（`问题，`）。
    assert_eq!(key(&mut router, 0xBC, ','), Some("问题，".to_owned()));
}

#[test]
fn unconverted_symbols_are_passthrough_when_not_composing() {
    // 非组句、不转全角的字符交还应用（既有契约）；竞态由 DLL 放行护栏兜底
    // （本地有未落定组句 / 编辑会话时吃下走提交队列，见 key_sink.rs）。
    let mut router = router(DICT);
    assert_eq!(
        outcome_at(&mut router, SessionId(1), 0xBD, '-'),
        KeyOutcome::Passthrough
    );
    assert_eq!(
        outcome_at(&mut router, SessionId(1), b'6' as u32, '6'),
        KeyOutcome::Passthrough
    );
}

#[test]
fn gpt_dash_six_sequence_keeps_order() {
    // 用户报告的原样复现：gpt-6 连打（6 用小键盘），符号与数字都进缓冲（候选呈现），空格一次上屏。
    let mut router = router(DICT);
    for c in "gpt".chars() {
        assert_eq!(key(&mut router, c as u32, c), None);
    }
    assert_eq!(key(&mut router, 0xBD, '-'), None);
    assert_eq!(key(&mut router, 0x66, '6'), None);
    assert_eq!(key(&mut router, 0x20, ' '), Some("gpt-6 ".to_owned()));
}

#[test]
fn console_host_passes_all_keys_through_without_composing() {
    // 终端 / 控制台类宿主（conhost）：任何字符键都不组句、逐键直通，
    // 宿主键盘层没有可吞的打开状态 → gpt-6 逐键原样输出。
    let mut router = router(DICT);
    let session = SessionId(2);
    assert!(matches!(
        router.handle(ClientMessage::OpenSession {
            session,
            app: Some("conhost.exe".to_owned()),
            protocol: PROTOCOL_VERSION,
        }),
        Some(ServerMessage::SessionOpened { .. })
    ));
    for c in "gpt-6".chars() {
        let vk = if c == '-' { 0xBD } else { c as u32 };
        assert_eq!(key_at(&mut router, session, vk, c), Some(c.to_string()));
    }
}
