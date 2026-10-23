//! 官员姓名的随机生成。
//!
//! 姓名在提议阶段生成并随 `AgentRegistered` 记入事件簿，因此回放不依赖随机数。
//! 生成器由调用方给定种子，测试可复现。

/// 常见单姓，大致按历代人口与史籍中出现的频率挑选。
const SURNAMES: &[&str] = &[
    "王", "李", "张", "刘", "陈", "杨", "黄", "赵", "周", "吴", "徐", "孙", "朱", "马", "胡", "郭",
    "林", "何", "高", "罗", "郑", "梁", "谢", "宋", "唐", "许", "韩", "冯", "邓", "曹", "彭", "曾",
    "萧", "田", "董", "潘", "袁", "蔡", "蒋", "余", "于", "杜", "叶", "程", "魏", "苏", "吕", "丁",
    "任", "沈", "姚", "卢", "姜", "崔", "钟", "谭", "陆", "汪", "范", "金", "石", "廖", "贾", "夏",
    "韦", "方", "白", "邹", "孟", "熊", "秦", "邱", "江", "尹", "薛", "闫", "段", "雷", "侯", "龙",
    "史", "陶", "黎", "贺", "顾", "毛", "郝", "龚", "邵", "万", "严", "覃", "武", "戴", "莫", "孔",
    "向", "汤", "杭", "申", "温", "柯", "钱", "倪", "鲁", "欧阳", "司马", "上官", "诸葛", "东方",
    "慕容", "令狐", "宇文",
];

/// 名中常用的字：多取自士大夫取名常见的德行、学问、山川与器物之字。
const GIVEN: &[&str] = &[
    "廷", "敬", "允", "懋", "时", "居", "正", "守", "仁", "孝", "文", "德", "明", "远", "之", "甫",
    "卿", "谦", "恭", "宗", "道", "学", "汝", "承", "光", "嗣", "秉", "端", "应", "春", "云", "鹤",
    "松", "溥", "鸿", "渊", "泰", "安", "璋", "瑾", "琦", "瑜", "达", "昭", "彦", "伯", "仲", "叔",
    "季", "子", "思", "贞", "良", "绍", "嘉", "靖", "宪", "章", "惟", "以", "希", "献", "怀", "节",
    "复", "初", "元", "亨", "利", "易", "诗", "书", "礼", "乐", "兴", "邦", "国", "臣", "辅", "弼",
    "济", "川", "峰", "岳", "海", "江", "清", "澄", "朗", "晖", "曜", "梦", "龙", "麟", "凤", "翔",
    "立", "本", "信", "义", "忠", "肃", "让", "直", "方", "圭", "珪", "璧", "宏", "镇", "铭", "锦",
    "韬", "略", "谟", "猷", "策", "化", "治", "鼎", "衡", "诚", "敏", "毅", "懿",
];

/// 种子决定序列的伪随机数（splitmix64）。
pub struct NameGenerator(u64);

impl NameGenerator {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self, bound: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z % bound as u64) as usize
    }

    /// 生成一个不满足 `taken` 的姓名：约四分之一为单名，其余为双名，名中两字不重复。
    pub fn name(&mut self, taken: impl Fn(&str) -> bool) -> String {
        loop {
            let mut name = SURNAMES[self.next(SURNAMES.len())].to_owned();
            let first = GIVEN[self.next(GIVEN.len())];
            name.push_str(first);
            if self.next(4) != 0 {
                let second = GIVEN[self.next(GIVEN.len())];
                if second == first {
                    continue;
                }
                name.push_str(second);
            }
            if !taken(&name) {
                return name;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn names_vary_and_avoid_taken_ones() {
        let mut generator = NameGenerator::new(7);
        let mut names = BTreeSet::new();
        for _ in 0..200 {
            let name = generator.name(|n| names.contains(n));
            assert!(names.insert(name));
        }
        // 不应退化为同一个姓或同一个末字（旧版的「X衡」「X工」）。
        let first_chars: BTreeSet<_> = names.iter().filter_map(|n| n.chars().next()).collect();
        let last_chars: BTreeSet<_> = names.iter().filter_map(|n| n.chars().last()).collect();
        assert!(first_chars.len() > 40, "{first_chars:?}");
        assert!(last_chars.len() > 60, "{last_chars:?}");
        assert!(names.iter().any(|n| n.chars().count() == 2));
        assert!(names.iter().any(|n| n.chars().count() == 3));
        // 同一种子产生同一序列。
        let mut a = NameGenerator::new(42);
        let mut b = NameGenerator::new(42);
        assert_eq!(a.name(|_| false), b.name(|_| false));
    }
}
