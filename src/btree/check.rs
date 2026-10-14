use std::collections::HashSet;

use crate::file::{
    pager::Pager,
    pages::{Page, table::TablePage, types::PageNumber},
};

pub(crate) struct TreeStats {
    pub depth: usize,
    pub rows: usize,
    pub leaves: usize,
    pub interiors: usize,
    pub pages: HashSet<PageNumber>,
}

pub(crate) fn check_tree(pager: &mut Pager, root: PageNumber) -> anyhow::Result<TreeStats> {
    let mut walk = Walk::default();
    walk.visit(pager, root, 0, None, None)?;
    Ok(TreeStats {
        depth: walk.leaf_depth.unwrap_or(0),
        rows: walk.rows,
        leaves: walk.leaves,
        interiors: walk.interiors,
        pages: walk.pages,
    })
}

#[derive(Default)]
struct Walk {
    leaf_depth: Option<usize>,
    last_key: Option<u64>,
    rows: usize,
    leaves: usize,
    interiors: usize,
    pages: HashSet<PageNumber>,
}

impl Walk {
    fn visit(
        &mut self,
        pager: &mut Pager,
        pn: PageNumber,
        depth: usize,
        lo: Option<u64>,
        hi: Option<u64>,
    ) -> anyhow::Result<()> {
        if !self.pages.insert(pn) {
            anyhow::bail!("page {pn} is reachable twice (there's a cycle or shared subtree)")
        }

        let ps = pager.spec(pn);
        let page = Page::parse(pager.get_page(pn)?, ps)?;

        match page {
            Page::TableLeaf(lp) => {
                self.leaves += 1;
                match self.leaf_depth {
                    None => self.leaf_depth = Some(depth),
                    Some(d) if d != depth => {
                        anyhow::bail!("leaf {pn} is at depth {depth}, others at {d}")
                    }
                    _ => {}
                }

                for cell in lp.cells() {
                    in_range(cell.row_id, lo, hi, pn)?;
                    if self.last_key.is_some_and(|key| cell.row_id <= key) {
                        anyhow::bail!("in leaf {pn} key {} is not in sorted order", cell.row_id)
                    }
                    self.last_key = Some(cell.row_id);
                    self.rows += 1;
                }
            }
            Page::TableInterior(ip) => {
                self.interiors += 1;
                let mut child_low = lo;

                for cell in ip.cells() {
                    in_range(cell.row_id, lo, hi, pn)?;
                    self.visit(
                        pager,
                        cell.left_child,
                        depth + 1,
                        child_low,
                        Some(cell.row_id),
                    )?;
                    child_low = Some(cell.row_id);
                }

                self.visit(pager, ip.rightmost_child, depth + 1, child_low, hi)?;
            }
        }

        Ok(())
    }
}

fn in_range(key: u64, lo: Option<u64>, hi: Option<u64>, pn: PageNumber) -> anyhow::Result<()> {
    if lo.is_some_and(|l| key <= l) || hi.is_some_and(|h| key > h) {
        anyhow::bail!("page {pn}: key {key} outside its subtree's range ({lo:?}, {hi:?}]");
    }
    Ok(())
}
