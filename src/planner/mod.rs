use crate::catalog::Catalog;

pub struct Planner {
    catalog: Catalog
}

impl Planner {
    pub fn new(catalog: Catalog) -> Self {
        Self { catalog }
    }

    
}