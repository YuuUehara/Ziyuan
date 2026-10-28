//! 官员与人设。

use serde::{Deserialize, Serialize};

use crate::AgentId;
use crate::office::Department;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub speciality: Department,
}

/// 官员的性格与文风，由官员 ID 决定：不记入事件簿，旧事件簿同样适用。
///
/// 人格只影响措辞与论证方式，不改变职权、玉玺与派工路线。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Persona {
    temperament: usize,
    style: usize,
}

/// 性格：名称与在提示词中的描述。
pub(crate) const TEMPERAMENTS: [(&str, &str); 6] = [
    ("刚直", "直言敢谏，见弊即言，不避忤旨"),
    ("持重", "老成持重，凡事先虑后患，主张稳妥"),
    ("锐进", "锐意进取，主张速办，敢于任事"),
    ("圆融", "长于调和，常求折中两全之策"),
    ("精细", "务求实证，好究细节，言必有据"),
    ("宏阔", "好论大体与长远，不拘小节"),
];

/// 文风：名称与在提示词中的描述。
pub(crate) const STYLES: [(&str, &str); 4] = [
    ("简劲", "文辞简劲，多用短句"),
    ("典雅", "好引经据典，多用四字句"),
    ("平实", "文风平实，如白话奏对"),
    ("条陈", "好分条陈述：一、二、三"),
];

impl Persona {
    pub fn of(agent: AgentId) -> Self {
        // splitmix64：相邻的 ID 也得到不相关的人格。
        let mut z = agent.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Self {
            temperament: (z % TEMPERAMENTS.len() as u64) as usize,
            style: ((z >> 32) % STYLES.len() as u64) as usize,
        }
    }

    /// 如「刚直·简劲」。
    pub fn label(self) -> String {
        format!(
            "{}·{}",
            TEMPERAMENTS[self.temperament].0, STYLES[self.style].0
        )
    }

    /// 写入官员系统提示词的一句话，并申明人格不越权。
    pub fn prompt(self) -> String {
        let (t, t_hint) = TEMPERAMENTS[self.temperament];
        let (s, s_hint) = STYLES[self.style];
        format!(
            "你性格{t}：{t_hint}；文风{s}：{s_hint}。\
             性格与文风只影响你的措辞与论证方式，不改变你的职权、玉玺与所述事实。"
        )
    }
}
