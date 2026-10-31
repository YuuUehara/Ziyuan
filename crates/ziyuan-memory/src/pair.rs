//! 按 ID 配对原文／译文段落，绝不按数组下标。
//!
//! 检查范围与 `ziyuan-app` 的 `validate_ids` 等价（缺段、重复、陌生、乱序、空译文、
//! 块 ID 不符、schema 版本），但结果按 ID 配好：候选顺序变了不会把译文配错段。
//! 任何结构问题都不返回部分结果——宁可整块不收录，也不产出错配的参考译法。
//!
//! 本模块只看结构，不判断译文质量。

use std::collections::{BTreeMap, BTreeSet};

/// 本 crate 认识提示块格式版本；更高或更低者一律不配对。
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// 段落原文视图：由 app 侧从 `BlockParagraph` 转换而来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParaPairInput {
    pub id: String,
    pub text: String,
}

/// 提示块的最小视图：由 app 侧从 `PromptBlock` 转换而来，故本 crate 不依赖 `ziyuan-app`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptBlockView {
    pub schema_version: u32,
    pub block_id: String,
    pub paragraphs: Vec<ParaPairInput>,
}

/// 一对已配好的原译文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphPair {
    pub id: String,
    pub source: String,
    pub target: String,
}

/// 配对遇到的结构问题。`position` 是候选中的位置，从 1 开始，与 `validate_ids` 一致。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingProblem {
    /// 候选的 schema 版本不受支持；此时只报这一条，其余字段不可信。
    UnsupportedVersion {
        found: u32,
    },
    WrongBlock {
        expected: String,
        found: String,
    },
    /// 原文有、候选没有的 ID，按原文顺序报出。
    Missing {
        id: String,
    },
    /// 候选里同一 ID 重复出现；每个 ID 只报第一次重复。
    Duplicate {
        id: String,
        position: usize,
    },
    /// 候选里出现了不属于此提示块的 ID。
    Unknown {
        id: String,
        position: usize,
    },
    /// 候选段落顺序与原文不符。
    OutOfOrder {
        id: String,
        position: usize,
    },
    /// 候选段落文本为空或只有空白。
    EmptyText {
        id: String,
        position: usize,
    },
}

/// 按 ID 配对，绝不按数组下标。任何结构问题都不返回部分结果。
///
/// 检查与 `ziyuan-app` 的 `validate_ids` 等价，因此**乱序本身**就是结构问题：
/// 候选段落顺序与原文不符时返回 [`PairingProblem::OutOfOrder`]，不会借机按下标配对。
/// 结构检查通过后，配对按 ID 查表（见 [`pairs_by_id`]），候选位置不参与决定谁配谁——
/// 即便将来放宽顺序检查，也不会把译文配错段。
pub fn pair_paragraphs(
    expected: &PromptBlockView,
    candidate: &PromptBlockView,
) -> Result<Vec<ParagraphPair>, Vec<PairingProblem>> {
    if candidate.schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err(vec![PairingProblem::UnsupportedVersion {
            found: candidate.schema_version,
        }]);
    }
    let mut problems = Vec::new();
    if candidate.block_id != expected.block_id {
        problems.push(PairingProblem::WrongBlock {
            expected: expected.block_id.clone(),
            found: candidate.block_id.clone(),
        });
    }
    let ranks: BTreeMap<&str, usize> = expected
        .paragraphs
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.as_str(), i))
        .collect();
    let present: BTreeSet<&str> = candidate.paragraphs.iter().map(|p| p.id.as_str()).collect();
    // 缺失按原文顺序；其余诊断按候选顺序。
    for paragraph in &expected.paragraphs {
        if !present.contains(paragraph.id.as_str()) {
            problems.push(PairingProblem::Missing {
                id: paragraph.id.clone(),
            });
        }
    }
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    let mut highest: Option<usize> = None;
    for (i, paragraph) in candidate.paragraphs.iter().enumerate() {
        let id = paragraph.id.as_str();
        let position = i + 1;
        let repeated = !seen.insert(id);
        if repeated && duplicates.insert(id) {
            problems.push(PairingProblem::Duplicate {
                id: id.to_owned(),
                position,
            });
        }
        match ranks.get(id).copied() {
            None => problems.push(PairingProblem::Unknown {
                id: id.to_owned(),
                position,
            }),
            Some(rank) if !repeated => {
                if highest.is_some_and(|highest| rank < highest) {
                    problems.push(PairingProblem::OutOfOrder {
                        id: id.to_owned(),
                        position,
                    });
                }
                highest = Some(highest.map_or(rank, |highest: usize| highest.max(rank)));
            }
            // 重复项不参与顺序判定，避免同一个 ID 连报两条。
            Some(_) => {}
        }
        if paragraph.text.trim().is_empty() {
            problems.push(PairingProblem::EmptyText {
                id: id.to_owned(),
                position,
            });
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    // 结构检查通过即 ID 集合与原文一致，余下只有按 ID 取译文这一件事。
    pairs_by_id(expected, candidate)
}

/// 按 ID 从候选里取译文，数组位置不参与配对；缺段即整块拒收，不产出半份结果。
///
/// 单独成函数是为了能直接证明「不是按下标 zip」：同一份候选换过顺序后，
/// 这里仍把每段配到它自己的 ID。
fn pairs_by_id(
    expected: &PromptBlockView,
    candidate: &PromptBlockView,
) -> Result<Vec<ParagraphPair>, Vec<PairingProblem>> {
    let targets: BTreeMap<&str, &str> = candidate
        .paragraphs
        .iter()
        .map(|p| (p.id.as_str(), p.text.as_str()))
        .collect();
    let mut pairs = Vec::with_capacity(expected.paragraphs.len());
    let mut missing = Vec::new();
    for paragraph in &expected.paragraphs {
        match targets.get(paragraph.id.as_str()) {
            Some(target) => pairs.push(ParagraphPair {
                id: paragraph.id.clone(),
                source: paragraph.text.clone(),
                target: (*target).to_owned(),
            }),
            None => missing.push(PairingProblem::Missing {
                id: paragraph.id.clone(),
            }),
        }
    }
    if missing.is_empty() {
        Ok(pairs)
    } else {
        // 丢弃已配好的部分，宁可整块不收。
        Err(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(block_id: &str, paragraphs: &[(&str, &str)]) -> PromptBlockView {
        PromptBlockView {
            schema_version: 1,
            block_id: block_id.to_owned(),
            paragraphs: paragraphs
                .iter()
                .map(|(id, text)| ParaPairInput {
                    id: (*id).to_owned(),
                    text: (*text).to_owned(),
                })
                .collect(),
        }
    }

    /// 与 `ziyuan-app` 的 `text_project/tests/text_project.rs` 同一套 ID 与块 ID。
    fn expected_block() -> PromptBlockView {
        block(
            "c0001-b0001",
            &[
                ("c0001-p0001", "One"),
                ("c0001-p0002", "Two"),
                ("c0001-p0003", "Three"),
                ("c0001-p0004", "Four"),
            ],
        )
    }

    fn problems(expected: &PromptBlockView, candidate: &PromptBlockView) -> Vec<PairingProblem> {
        match pair_paragraphs(expected, candidate) {
            Ok(pairs) => panic!("预期结构不合格，得到 {pairs:?}"),
            Err(problems) => problems,
        }
    }

    fn pair(id: &str, source: &str, target: &str) -> ParagraphPair {
        ParagraphPair {
            id: id.to_owned(),
            source: source.to_owned(),
            target: target.to_owned(),
        }
    }

    #[test]
    fn pairs_in_expected_order_for_a_well_formed_candidate() {
        let expected = block("b1", &[("p1", "One"), ("p2", "Two"), ("p3", "Three")]);
        let candidate = block("b1", &[("p1", "一"), ("p2", "二"), ("p3", "三")]);
        assert_eq!(
            pair_paragraphs(&expected, &candidate).unwrap(),
            vec![
                pair("p1", "One", "一"),
                pair("p2", "Two", "二"),
                pair("p3", "Three", "三"),
            ]
        );
    }

    /// 直接证明配对按 ID 查表、不按下标 zip：候选顺序全变，每段仍配到自己的译文。
    ///
    /// `validate_ids` 把乱序视为结构问题，所以 `pair_paragraphs` 对这一候选返回
    /// `OutOfOrder`（见下一个用例）；配对核心本身与位置无关这件事由本用例守住。
    #[test]
    fn pairs_by_id_ignores_candidate_positions() {
        let expected = block("b1", &[("p1", "One"), ("p2", "Two"), ("p3", "Three")]);
        let candidate = block("b1", &[("p3", "三"), ("p1", "一"), ("p2", "二")]);
        let pairs = pairs_by_id(&expected, &candidate).unwrap();
        assert_eq!(
            pairs,
            vec![
                pair("p1", "One", "一"),
                pair("p2", "Two", "二"),
                pair("p3", "Three", "三"),
            ]
        );
        // 按下标 zip 会把 p1 配到「三」、p2 配到「一」、p3 配到「二」。
        assert_eq!(pairs[0].target, "一");
        assert_eq!(pairs[1].target, "二");
        assert_eq!(pairs[2].target, "三");
    }

    /// 顺序不符本身就是结构问题，整块不收——绝不会退化成按下标硬配。
    #[test]
    fn reordered_candidate_is_rejected_as_out_of_order() {
        let expected = block("b1", &[("p1", "One"), ("p2", "Two"), ("p3", "Three")]);
        let candidate = block("b1", &[("p3", "三"), ("p1", "一"), ("p2", "二")]);
        assert_eq!(
            problems(&expected, &candidate),
            vec![
                PairingProblem::OutOfOrder {
                    id: "p1".into(),
                    position: 2,
                },
                PairingProblem::OutOfOrder {
                    id: "p2".into(),
                    position: 3,
                },
            ]
        );
    }

    #[test]
    fn missing_ids_follow_expected_order() {
        let problems = problems(
            &expected_block(),
            &block("c0001-b0001", &[("c0001-p0001", "一")]),
        );
        assert_eq!(
            problems,
            vec![
                PairingProblem::Missing {
                    id: "c0001-p0002".into()
                },
                PairingProblem::Missing {
                    id: "c0001-p0003".into()
                },
                PairingProblem::Missing {
                    id: "c0001-p0004".into()
                },
            ]
        );
    }

    #[test]
    fn three_occurrences_report_one_duplicate() {
        let expected = block("b1", &[("p1", "One"), ("p2", "Two")]);
        let candidate = block(
            "b1",
            &[("p1", "一"), ("p1", "甲"), ("p1", "乙"), ("p2", "二")],
        );
        let problems = problems(&expected, &candidate);
        assert_eq!(
            problems,
            vec![PairingProblem::Duplicate {
                id: "p1".into(),
                position: 2,
            }]
        );
    }

    #[test]
    fn unknown_id_is_reported() {
        let expected = block("b1", &[("p1", "One")]);
        let candidate = block("b1", &[("p1", "一"), ("p9", "别章")]);
        assert_eq!(
            problems(&expected, &candidate),
            vec![PairingProblem::Unknown {
                id: "p9".into(),
                position: 2,
            }]
        );
    }

    #[test]
    fn out_of_order_is_reported() {
        let expected = block("b1", &[("p1", "One"), ("p2", "Two"), ("p3", "Three")]);
        let candidate = block("b1", &[("p2", "二"), ("p1", "一"), ("p3", "三")]);
        assert_eq!(
            problems(&expected, &candidate),
            vec![PairingProblem::OutOfOrder {
                id: "p1".into(),
                position: 2,
            }]
        );
    }

    #[test]
    fn empty_text_is_reported() {
        let expected = block("b1", &[("p1", "One"), ("p2", "Two")]);
        let candidate = block("b1", &[("p1", "一"), ("p2", "　 \r\n")]);
        assert_eq!(
            problems(&expected, &candidate),
            vec![PairingProblem::EmptyText {
                id: "p2".into(),
                position: 2,
            }]
        );
    }

    #[test]
    fn wrong_block_is_reported_alongside_other_problems() {
        let expected = block("c0001-b0001", &[("p1", "One"), ("p2", "Two")]);
        let candidate = block("c0002-b0001", &[("p1", "一")]);
        assert_eq!(
            problems(&expected, &candidate),
            vec![
                PairingProblem::WrongBlock {
                    expected: "c0001-b0001".into(),
                    found: "c0002-b0001".into(),
                },
                PairingProblem::Missing { id: "p2".into() },
            ]
        );
    }

    #[test]
    fn newer_version_reports_only_the_version() {
        let mut candidate = block("c0001-b0001", &[("p1", " "), ("p1", "一")]);
        candidate.schema_version = 2;
        assert_eq!(
            problems(&expected_block(), &candidate),
            vec![PairingProblem::UnsupportedVersion { found: 2 }]
        );
        let mut candidate = block("c0001-b0001", &[]);
        candidate.schema_version = 0;
        assert_eq!(
            problems(&expected_block(), &candidate),
            vec![PairingProblem::UnsupportedVersion { found: 0 }]
        );
    }

    #[test]
    fn empty_expected_block_pairs_to_nothing() {
        let expected = block("b1", &[]);
        let candidate = block("b1", &[]);
        assert_eq!(pair_paragraphs(&expected, &candidate), Ok(Vec::new()));
    }

    /// `candidate_collects_missing_duplicate_order_unknown_and_empty_together`
    /// （`crates/ziyuan-app/tests/text_project.rs`）的同一份候选，
    /// 断言问题种类与顺序与 `validate_ids` 一致。
    #[test]
    fn problems_match_validate_ids_fixture_in_kind_and_order() {
        let candidate = block(
            "c0001-b0001",
            &[
                ("c0001-p0002", "二"),
                ("c0001-p0001", "一"),
                ("c0001-p0002", "重复"),
                ("c0002-p0001", "别章"),
                ("c0001-p0004", "　 \r\n"),
            ],
        );
        assert_eq!(
            problems(&expected_block(), &candidate),
            vec![
                PairingProblem::Missing {
                    id: "c0001-p0003".into()
                },
                PairingProblem::OutOfOrder {
                    id: "c0001-p0001".into(),
                    position: 2,
                },
                PairingProblem::Duplicate {
                    id: "c0001-p0002".into(),
                    position: 3,
                },
                PairingProblem::Unknown {
                    id: "c0002-p0001".into(),
                    position: 4,
                },
                PairingProblem::EmptyText {
                    id: "c0001-p0004".into(),
                    position: 5,
                },
            ]
        );
    }
}
