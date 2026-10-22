use std::{collections::HashSet, fmt::Debug};

use crate::storage::{
    pager::Pager,
    pages::{
        Page,
        btree_page::BTreePage,
        cell::{Cell, InteriorCell},
        types::{PageNumber, RowID},
    },
    record::values::Value,
};

/// A child page to recurse into, with the key-range it must stay inside.
type Child<Key> = (PageNumber, Option<Key>, Option<Key>);

pub struct TreeStats {
    pub depth: usize,
    pub rows: usize,
    pub leaves: usize,
    pub interiors: usize,
    pub pages: HashSet<PageNumber>,
}

pub fn check_table_tree(pager: &mut Pager, root: PageNumber) -> anyhow::Result<TreeStats> {
    let mut walk = Walk::<RowID>::default();
    walk.visit_table(pager, root, 0, None, None)?;
    Ok(walk.into_stats())
}

pub fn check_index_tree(pager: &mut Pager, root: PageNumber) -> anyhow::Result<TreeStats> {
    let mut walk = Walk::<(Vec<Value>, RowID)>::default();
    walk.visit_index(pager, root, 0, None, None)?;
    Ok(walk.into_stats())
}

struct Walk<Key: Ord + Debug + Clone> {
    leaf_depth: Option<usize>,
    last_key: Option<Key>,
    rows: usize,
    leaves: usize,
    interiors: usize,
    pages: HashSet<PageNumber>,
}

impl<Key: Ord + Debug + Clone> Default for Walk<Key> {
    fn default() -> Self {
        Self {
            leaf_depth: None,
            last_key: None,
            rows: 0,
            leaves: 0,
            interiors: 0,
            pages: HashSet::new(),
        }
    }
}

impl<Key: Ord + Debug + Clone> Walk<Key> {
    fn into_stats(self) -> TreeStats {
        TreeStats {
            depth: self.leaf_depth.unwrap_or(0),
            rows: self.rows,
            leaves: self.leaves,
            interiors: self.interiors,
            pages: self.pages,
        }
    }

    fn mark_reachable(&mut self, pn: PageNumber) -> anyhow::Result<()> {
        if !self.pages.insert(pn) {
            anyhow::bail!("page {pn} is reachable twice (there's a cycle or shared subtree)")
        }
        Ok(())
    }

    fn leaf_check<P>(
        &mut self,
        page: &P,
        depth: usize,
        lo: Option<&Key>,
        hi: Option<&Key>,
        pn: PageNumber,
    ) -> anyhow::Result<()>
    where
        P: BTreePage<Key = Key>,
        P::C: Cell<Key = Key>,
    {
        self.leaves += 1;
        match self.leaf_depth {
            None => self.leaf_depth = Some(depth),
            Some(d) if d != depth => {
                anyhow::bail!("leaf {pn} is at depth {depth}, others at {d}")
            }
            _ => {}
        }

        for cell in page.cells() {
            let key = cell.sort_key();
            Self::in_range(&key, lo, hi, pn)?;
            if self.last_key.as_ref().is_some_and(|last| &key <= last) {
                anyhow::bail!("in leaf {pn} key {key:?} is not in sorted order")
            }
            self.last_key = Some(key);
            self.rows += 1;
        }

        Ok(())
    }

    /// Validates an interior page's dividers and returns the `(child, lo, hi)`
    /// triples the caller should recurse into. Doesn't recurse itself, since
    /// the recursive call (`visit_table` vs `visit_index`) is family-specific
    /// while this check is shared by both.
    fn interior_check<P>(
        &mut self,
        page: &P,
        lo: Option<Key>,
        hi: Option<Key>,
        pn: PageNumber,
    ) -> anyhow::Result<Vec<Child<Key>>>
    where
        P: BTreePage<Key = Key>,
        P::C: InteriorCell<Key = Key>,
    {
        self.interiors += 1;
        let mut out = Vec::with_capacity(page.cells().len() + 1);
        let mut child_low = lo.clone();

        for cell in page.cells() {
            let key = cell.sort_key();
            Self::in_range(&key, lo.as_ref(), hi.as_ref(), pn)?;
            out.push((cell.left_child(), child_low, Some(key.clone())));
            child_low = Some(key);
        }

        let rightmost = page
            .rightmost_child()
            .ok_or_else(|| anyhow::anyhow!("interior page {pn} has no rightmost child"))?;
        out.push((rightmost, child_low, hi));

        Ok(out)
    }

    fn in_range(
        key: &Key,
        lo: Option<&Key>,
        hi: Option<&Key>,
        pn: PageNumber,
    ) -> anyhow::Result<()> {
        if lo.is_some_and(|l| key <= l) || hi.is_some_and(|h| key > h) {
            anyhow::bail!("page {pn}: key {key:?} outside its subtree's range ({lo:?}, {hi:?}]");
        }
        Ok(())
    }
}

// `visit_table`/`visit_index` are pinned to one concrete `Key` each (rather than
// living in `impl<Key> Walk<Key>`) because each one matches on `Page`'s full set
// of variants, including the other family's — which only type-checks once `Key`
// is fixed to the one page family it actually handles.
impl Walk<RowID> {
    fn visit_table(
        &mut self,
        pager: &mut Pager,
        pn: PageNumber,
        depth: usize,
        lo: Option<RowID>,
        hi: Option<RowID>,
    ) -> anyhow::Result<()> {
        self.mark_reachable(pn)?;

        let ps = pager.spec(pn);
        let page = Page::parse(pager.get_page(pn)?, ps)?;

        match page {
            Page::TableLeaf(lp) => self.leaf_check(&lp, depth, lo.as_ref(), hi.as_ref(), pn),
            Page::TableInterior(ip) => {
                let children = self.interior_check(&ip, lo, hi, pn)?;
                for (child, child_lo, child_hi) in children {
                    self.visit_table(pager, child, depth + 1, child_lo, child_hi)?;
                }
                Ok(())
            }
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                anyhow::bail!("page {pn}: expected a table page, found an index page")
            }
        }
    }
}

impl Walk<(Vec<Value>, RowID)> {
    fn visit_index(
        &mut self,
        pager: &mut Pager,
        pn: PageNumber,
        depth: usize,
        lo: Option<(Vec<Value>, RowID)>,
        hi: Option<(Vec<Value>, RowID)>,
    ) -> anyhow::Result<()> {
        self.mark_reachable(pn)?;

        let ps = pager.spec(pn);
        let page = Page::parse(pager.get_page(pn)?, ps)?;

        match page {
            Page::IndexLeaf(lp) => self.leaf_check(&lp, depth, lo.as_ref(), hi.as_ref(), pn),
            Page::IndexInterior(ip) => {
                let children = self.interior_check(&ip, lo, hi, pn)?;
                for (child, child_lo, child_hi) in children {
                    self.visit_index(pager, child, depth + 1, child_lo, child_hi)?;
                }
                Ok(())
            }
            Page::TableLeaf(_) | Page::TableInterior(_) => {
                anyhow::bail!("page {pn}: expected an index page, found a table page")
            }
        }
    }
}
