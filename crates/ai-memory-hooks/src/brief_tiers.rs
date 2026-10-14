//! Tier assignment for the SessionStart project brief.
//!
//! The brief used to stream full page bodies until the budget ran out, so
//! one long `_rules/` page truncated and every later standing page was
//! reduced to an "omitted" pointer. Assignment here is coverage-first: every
//! page that fits gets its L0 excerpt (or a bare path line when it has none),
//! then whatever budget is left deepens pages to L1 and full L2 bodies,
//! privileged (pinned or `_slots/`) pages first. An L2 is taken only whole,
//! never truncated.
//!
//! This module only does the arithmetic over rendered byte costs; the hook
//! router owns the strings and the security scaffold around them.

/// How deep a core page renders in the brief. Ordered by depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BriefTier {
    /// Crowded out: listed in the omitted section by path only.
    Omitted,
    /// No excerpt to show: rendered as a bare path line.
    TitleOnly,
    /// Heading plus the short excerpt.
    L0,
    /// Heading plus the longer excerpt.
    L1,
    /// Heading plus the whole body.
    L2,
}

/// Rendered byte cost of one core page at each depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageCosts {
    /// Pinned or `_slots/`: deepened before every other page.
    pub privileged: bool,
    /// Cost as a bare path line.
    pub title_only: usize,
    /// Costs at L0, L1 and L2, or `None` when the page has no excerpt and
    /// can only render as a path line.
    pub tiers: Option<[usize; 3]>,
}

impl PageCosts {
    fn cost(&self, tier: BriefTier) -> usize {
        match (tier, self.tiers) {
            (BriefTier::Omitted, _) => 0,
            (BriefTier::L0, Some([l0, _, _])) => l0,
            (BriefTier::L1, Some([_, l1, _])) => l1,
            (BriefTier::L2, Some([_, _, l2])) => l2,
            _ => self.title_only,
        }
    }

    /// Whether deepening to L1 would show anything L0 does not.
    fn l1_adds(&self) -> bool {
        self.tiers.is_some_and(|[l0, l1, _]| l1 > l0)
    }
}

/// The fixed parts of the budget the pages compete inside.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BriefBudget {
    /// Bytes already spent before the first page (preamble, notice, fence).
    pub prefix: usize,
    /// The budget minus everything from the closing fence onward.
    pub content_ceiling: usize,
    /// The recent-pages section at full length.
    pub recent_full: usize,
}

/// The assignment: a tier per core page and what is held back for the
/// sections rendered after the pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BriefPlan {
    /// One tier per core page, in store order. The omitted pages are always
    /// a suffix.
    pub tiers: Vec<BriefTier>,
    /// Index of the first omitted page (`tiers.len()` when none is).
    pub omitted_from: usize,
    /// Bytes the recent section keeps when the omitted section is rendered.
    pub recent_reserve: usize,
}

/// Assign a tier to every core page.
///
/// `omitted_full(from)` is the length of the omitted section listing every
/// page from `from` on, and `count_only(n)` the length of its shortest form,
/// the bare count of `n` pages.
pub(crate) fn assign_tiers(
    pages: &[PageCosts],
    budget: BriefBudget,
    omitted_full: impl Fn(usize) -> usize,
    count_only: impl Fn(usize) -> usize,
) -> BriefPlan {
    let mut tiers = vec![BriefTier::Omitted; pages.len()];
    let variable = budget.content_ceiling.saturating_sub(budget.prefix);
    let recent_cap = budget.recent_full.min(variable / 4);
    // A crowded-out page is a standing rule the agent did not receive, so
    // the count of omitted pages is reserved before coverage starts. It is
    // sized for every page being omitted, which is the most digits it needs.
    let omitted_floor = if pages.is_empty() {
        0
    } else {
        count_only(pages.len())
    };
    let coverage_ceiling = budget
        .content_ceiling
        .saturating_sub(recent_cap + omitted_floor);

    let mut spent = budget.prefix;
    let mut omitted_from = pages.len();
    for (idx, page) in pages.iter().enumerate() {
        let tier = if page.tiers.is_some() {
            BriefTier::L0
        } else {
            BriefTier::TitleOnly
        };
        let cost = page.cost(tier);
        if spent + cost > coverage_ceiling {
            omitted_from = idx;
            break;
        }
        tiers[idx] = tier;
        spent += cost;
    }

    let remaining = budget.content_ceiling.saturating_sub(spent);
    let omitted_reserve = if omitted_from == pages.len() {
        0
    } else {
        omitted_full(omitted_from)
            .min(remaining / 4)
            .max(count_only(pages.len() - omitted_from))
            .min(remaining)
    };
    let recent_reserve = budget
        .recent_full
        .min(remaining.saturating_sub(omitted_reserve) / 4);
    let mut room = remaining.saturating_sub(omitted_reserve + recent_reserve);

    let included = &pages[..omitted_from];
    let mut promote = |tiers: &mut [BriefTier], privileged: bool, target: BriefTier| {
        for (idx, page) in included.iter().enumerate() {
            let current = tiers[idx];
            if page.privileged != privileged
                || page.tiers.is_none()
                || current >= target
                || current == BriefTier::TitleOnly
            {
                continue;
            }
            let (from, to) = (page.cost(current), page.cost(target));
            if to > from && to - from <= room {
                tiers[idx] = target;
                room -= to - from;
            }
        }
    };
    promote(&mut tiers, true, BriefTier::L1);
    promote(&mut tiers, true, BriefTier::L2);
    promote(&mut tiers, false, BriefTier::L1);
    // Full bodies for the remaining pages only once every included page
    // shows as much as its L1 can: a long body must not outbid a summary.
    let every_page_at_l1 = included
        .iter()
        .zip(&tiers)
        .all(|(page, tier)| *tier >= BriefTier::L1 || !page.l1_adds());
    if every_page_at_l1 {
        promote(&mut tiers, false, BriefTier::L2);
    }

    BriefPlan {
        tiers,
        omitted_from,
        recent_reserve,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BriefTier::{L0, L1, L2, Omitted, TitleOnly};

    const COUNT_ONLY: usize = 60;
    const LISTED: usize = 30;

    fn page(privileged: bool, l0: usize, l1: usize, l2: usize) -> PageCosts {
        PageCosts {
            privileged,
            title_only: 20,
            tiers: Some([l0, l1, l2]),
        }
    }

    fn assign(pages: &[PageCosts], content_ceiling: usize, recent_full: usize) -> BriefPlan {
        assign_tiers(
            pages,
            BriefBudget {
                prefix: 100,
                content_ceiling,
                recent_full,
            },
            |from| 40 + LISTED * (pages.len() - from),
            |_| COUNT_ONLY,
        )
    }

    fn spent(pages: &[PageCosts], plan: &BriefPlan) -> usize {
        pages.iter().zip(&plan.tiers).map(|(p, t)| p.cost(*t)).sum()
    }

    #[test]
    fn a_long_first_body_does_not_crowd_out_later_pages() {
        let pages = [page(false, 100, 400, 5_000), page(false, 100, 400, 300)];
        let plan = assign(&pages, 1_000, 0);
        assert_eq!(plan.omitted_from, 2, "both pages are covered");
        assert!(plan.tiers.iter().all(|t| *t >= L0), "{plan:?}");
        assert_ne!(
            plan.tiers[0], L2,
            "a body that does not fit whole stays an excerpt"
        );
    }

    #[test]
    fn coverage_keeps_room_for_the_omitted_count() {
        let pages = vec![page(false, 150, 150, 150); 24];
        let ceiling = 2_000;
        let plan = assign(&pages, ceiling, 0);
        assert!(
            (1..24).contains(&plan.omitted_from),
            "some but not every page fits: {plan:?}"
        );
        assert!(
            plan.tiers[plan.omitted_from..]
                .iter()
                .all(|t| *t == Omitted),
            "omitted pages are a suffix"
        );
        assert!(
            100 + spent(&pages, &plan) + COUNT_ONLY <= ceiling,
            "the omitted count still fits after coverage"
        );
    }

    #[test]
    fn privileged_pages_deepen_first() {
        let pages = [
            page(false, 100, 400, 900),
            page(true, 100, 400, 900),
            page(false, 100, 400, 900),
        ];
        // Room for the three L0s plus one body.
        let plan = assign(&pages, 100 + 300 + 800, 0);
        assert_eq!(plan.tiers, vec![L0, L2, L0]);
    }

    #[test]
    fn summaries_come_before_unprivileged_bodies() {
        let pages = [page(false, 100, 400, 450), page(false, 100, 400, 450)];
        // Room for both L1s but not for a body on top of them.
        let plan = assign(&pages, 100 + 800 + 40, 0);
        assert_eq!(plan.tiers, vec![L1, L1]);
        // One L1 does not fit: no unprivileged body may jump the queue.
        let pages = [page(false, 100, 900, 950), page(false, 100, 150, 160)];
        let plan = assign(&pages, 100 + 200 + 100, 0);
        assert_eq!(plan.tiers, vec![L0, L1]);
    }

    #[test]
    fn a_small_wiki_gets_every_body_whole() {
        let pages = [
            page(false, 80, 80, 120),
            page(false, 60, 90, 200),
            page(true, 50, 50, 300),
        ];
        let plan = assign(&pages, 4_000, 500);
        assert_eq!(plan.tiers, vec![L2, L2, L2]);
        assert_eq!(plan.omitted_from, 3);
    }

    #[test]
    fn zero_delta_and_title_only_pages_do_not_block_bodies() {
        let pages = [
            PageCosts {
                privileged: false,
                title_only: 20,
                tiers: None,
            },
            page(false, 50, 50, 50),
            page(false, 50, 50, 400),
        ];
        let plan = assign(&pages, 4_000, 0);
        assert_eq!(plan.tiers, vec![TitleOnly, L0, L2]);
    }

    #[test]
    fn recent_titles_keep_a_share_of_what_is_left() {
        let pages = [page(false, 100, 100, 10_000)];
        let plan = assign(&pages, 2_000, 600);
        assert!(plan.recent_reserve > 0, "{plan:?}");
        let pages = [page(false, 100, 100, 10_000)];
        let plan = assign(&pages, 2_000, 0);
        assert_eq!(
            plan.recent_reserve, 0,
            "nothing held back when nothing is recent"
        );
    }

    #[test]
    fn no_pages_is_an_empty_plan() {
        let plan = assign(&[], 4_000, 100);
        assert_eq!(plan.tiers, Vec::<BriefTier>::new());
        assert_eq!(plan.omitted_from, 0);
    }
}
