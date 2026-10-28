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

/// 官员的字与号的默认值，由官员 ID 决定（同 [`Persona`]），不记入事件簿；皇帝可以改易
/// （`CourtEvent::AgentRenamed`），改过的以事件簿为准。返回 `(字, 号)`，如 `("元玉", "天全")`。
pub fn default_styles(agent: AgentId) -> (String, String) {
    const COURTESY: [&str; 20] = [
        "元", "伯", "仲", "叔", "季", "子", "德", "文", "汝", "惟", "廷", "允", "时", "思", "景",
        "彦", "孟", "希", "克", "用",
    ];
    const COURTESY_TAIL: [&str; 20] = [
        "玉", "之", "甫", "卿", "瞻", "晦", "明", "远", "正", "贞", "敬", "道", "中", "和", "章",
        "度", "实", "吉", "举", "衡",
    ];
    const ART: [&str; 24] = [
        "松", "竹", "石", "云", "月", "梅", "兰", "泉", "樵", "渔", "鹤", "荷", "南", "东", "西",
        "半", "一", "天", "静", "拙", "青", "白", "鹿", "晚",
    ];
    const ART_TAIL: [&str; 12] = [
        "庵", "斋", "山", "溪", "轩", "居士", "山人", "堂", "峰", "川", "全", "翁",
    ];
    // splitmix64，常数与人格不同：字号与人格互不相关。
    let mut z = agent.0.wrapping_add(0x3C6E_F372_FE94_F82B);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let pick = |pool: &[&'static str], shift: u32| -> &'static str {
        pool[((z >> shift) % pool.len() as u64) as usize]
    };
    (
        format!("{}{}", pick(&COURTESY, 0), pick(&COURTESY_TAIL, 16)),
        format!("{}{}", pick(&ART, 32), pick(&ART_TAIL, 48)),
    )
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
