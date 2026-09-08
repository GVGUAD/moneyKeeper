use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::shared_kernel::UserId;

use super::public::{
    CategoryId, CategoryKind, CategoryLifecycle, ClassificationError, ICON_CATALOG,
};

pub(crate) const STARTER_TEMPLATE_VERSION: i32 = 1;
pub(crate) const DEFAULT_COLOR: &str = "#64748B";
pub(crate) const DEFAULT_ICON: &str = "tag";
const MAX_DEPTH: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Category {
    id: CategoryId,
    user_id: UserId,
    name: String,
    kind: CategoryKind,
    lifecycle: CategoryLifecycle,
    parent_id: Option<CategoryId>,
    position: i32,
    color: Option<String>,
    icon: Option<String>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Category {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create(
        id: CategoryId,
        user_id: UserId,
        name: String,
        kind: CategoryKind,
        parent_id: Option<CategoryId>,
        position: i32,
        color: Option<String>,
        icon: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, ClassificationError> {
        Ok(Self {
            id,
            user_id,
            name: validate_name(name)?,
            kind,
            lifecycle: CategoryLifecycle::Active,
            parent_id,
            position,
            color: validate_color(color)?,
            icon: validate_icon(icon)?,
            version: 1,
            created_at: now,
            updated_at: now,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reconstitute(
        id: CategoryId,
        user_id: UserId,
        name: String,
        kind: CategoryKind,
        lifecycle: CategoryLifecycle,
        parent_id: Option<CategoryId>,
        position: i32,
        color: Option<String>,
        icon: Option<String>,
        version: i64,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, ClassificationError> {
        if version < 1 || position < 0 {
            return Err(ClassificationError::persistence(
                "stored category version or position is invalid",
            ));
        }
        Ok(Self {
            id,
            user_id,
            name: validate_name(name)
                .map_err(|_| ClassificationError::persistence("stored category name is invalid"))?,
            kind,
            lifecycle,
            parent_id,
            position,
            color: validate_color(color).map_err(|_| {
                ClassificationError::persistence("stored category color is invalid")
            })?,
            icon: validate_icon(icon)
                .map_err(|_| ClassificationError::persistence("stored category icon is invalid"))?,
            version,
            created_at,
            updated_at,
        })
    }

    fn update_details(
        &mut self,
        name: Option<String>,
        color: Option<Option<String>>,
        icon: Option<Option<String>>,
        now: DateTime<Utc>,
    ) -> Result<(), ClassificationError> {
        if let Some(name) = name {
            self.name = validate_name(name)?;
        }
        if let Some(color) = color {
            self.color = validate_color(color)?;
        }
        if let Some(icon) = icon {
            self.icon = validate_icon(icon)?;
        }
        self.bump(now);
        Ok(())
    }

    fn bump(&mut self, now: DateTime<Utc>) {
        self.version += 1;
        self.updated_at = now;
    }

    pub(crate) fn id(&self) -> CategoryId {
        self.id
    }
    pub(crate) fn user_id(&self) -> UserId {
        self.user_id
    }
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn kind(&self) -> CategoryKind {
        self.kind
    }
    pub(crate) fn lifecycle(&self) -> CategoryLifecycle {
        self.lifecycle
    }
    pub(crate) fn parent_id(&self) -> Option<CategoryId> {
        self.parent_id
    }
    pub(crate) fn position(&self) -> i32 {
        self.position
    }
    pub(crate) fn color(&self) -> Option<&str> {
        self.color.as_deref()
    }
    pub(crate) fn icon(&self) -> Option<&str> {
        self.icon.as_deref()
    }
    pub(crate) fn version(&self) -> i64 {
        self.version
    }
    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub(crate) fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CategoryTaxonomy {
    user_id: UserId,
    version: i64,
    starter_template_version: Option<i32>,
    categories: Vec<Category>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl CategoryTaxonomy {
    pub(crate) fn starter(user_id: UserId, now: DateTime<Utc>) -> Self {
        let mut taxonomy = Self {
            user_id,
            version: 1,
            starter_template_version: Some(STARTER_TEMPLATE_VERSION),
            categories: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        taxonomy.insert_starter_nodes(now);
        taxonomy
    }

    pub(crate) fn for_existing(
        user_id: UserId,
        categories: Vec<Category>,
        now: DateTime<Utc>,
    ) -> Result<Self, ClassificationError> {
        let taxonomy = Self {
            user_id,
            version: 1,
            starter_template_version: None,
            categories,
            created_at: now,
            updated_at: now,
        };
        taxonomy.validate_stored()?;
        Ok(taxonomy)
    }

    pub(crate) fn reconstitute(
        user_id: UserId,
        version: i64,
        starter_template_version: Option<i32>,
        categories: Vec<Category>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, ClassificationError> {
        if version < 1 || starter_template_version.is_some_and(|value| value < 1) {
            return Err(ClassificationError::persistence(
                "stored category taxonomy version is invalid",
            ));
        }
        let taxonomy = Self {
            user_id,
            version,
            starter_template_version,
            categories,
            created_at,
            updated_at,
        };
        taxonomy.validate_stored()?;
        Ok(taxonomy)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_node(
        &mut self,
        expected_version: i64,
        name: String,
        kind: CategoryKind,
        parent_id: Option<CategoryId>,
        position: Option<i32>,
        color: Option<String>,
        icon: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<CategoryId, ClassificationError> {
        if let Some(parent) = parent_id {
            self.category(parent)?;
        }
        self.require_version(expected_version)?;
        let mut next = self.clone();
        next.require_parent_accepts(parent_id, kind)?;
        let sibling_count = next.children_ids(parent_id).len() as i32;
        let position = position.unwrap_or(sibling_count);
        if !(0..=sibling_count).contains(&position) {
            return Err(ClassificationError::invalid_structure(
                "category position is outside the sibling range",
            ));
        }
        let id = CategoryId::generate();
        next.categories.push(Category::create(
            id,
            self.user_id,
            name,
            kind,
            parent_id,
            position,
            color,
            icon,
            now,
        )?);
        next.insert_at(parent_id, id, position as usize);
        next.validate_command()?;
        next.bump(now);
        *self = next;
        Ok(id)
    }

    pub(crate) fn update_node(
        &mut self,
        expected_version: i64,
        id: CategoryId,
        name: Option<String>,
        color: Option<Option<String>>,
        icon: Option<Option<String>>,
        now: DateTime<Utc>,
    ) -> Result<(), ClassificationError> {
        self.category(id)?;
        self.require_version(expected_version)?;
        if name.is_none() && color.is_none() && icon.is_none() {
            return Err(ClassificationError::invalid_structure(
                "at least one category field must be supplied",
            ));
        }
        let mut next = self.clone();
        next.category_mut(id)?
            .update_details(name, color, icon, now)?;
        next.validate_command()?;
        next.bump(now);
        *self = next;
        Ok(())
    }

    pub(crate) fn move_node(
        &mut self,
        expected_version: i64,
        id: CategoryId,
        parent_id: Option<CategoryId>,
        target_position: i32,
        now: DateTime<Utc>,
    ) -> Result<(), ClassificationError> {
        self.category(id)?;
        if let Some(parent) = parent_id {
            self.category(parent)?;
        }
        self.require_version(expected_version)?;
        let mut next = self.clone();
        let node = next.category(id)?.clone();
        if parent_id == Some(id)
            || parent_id.is_some_and(|parent| next.subtree_ids(id).contains(&parent))
        {
            return Err(ClassificationError::invalid_structure(
                "a category cannot be moved into its own subtree",
            ));
        }
        next.require_parent_accepts(parent_id, node.kind())?;
        let old_parent = node.parent_id();
        let destination_len = next
            .children_ids(parent_id)
            .into_iter()
            .filter(|candidate| *candidate != id)
            .count() as i32;
        if !(0..=destination_len).contains(&target_position) {
            return Err(ClassificationError::invalid_structure(
                "category position is outside the sibling range",
            ));
        }
        next.remove_from_siblings(old_parent, id);
        next.category_mut(id)?.parent_id = parent_id;
        next.insert_at(parent_id, id, target_position as usize);
        next.category_mut(id)?.bump(now);
        next.validate_command()?;
        next.bump(now);
        *self = next;
        Ok(())
    }

    pub(crate) fn reorder(
        &mut self,
        expected_version: i64,
        parent_id: Option<CategoryId>,
        ordered_ids: Vec<CategoryId>,
        now: DateTime<Utc>,
    ) -> Result<(), ClassificationError> {
        if let Some(parent) = parent_id {
            self.category(parent)?;
        }
        self.require_version(expected_version)?;
        let mut next = self.clone();
        if let Some(parent_id) = parent_id {
            next.category(parent_id)?;
        }
        let current = next.children_ids(parent_id);
        let current_set: HashSet<_> = current.iter().copied().collect();
        let ordered_set: HashSet<_> = ordered_ids.iter().copied().collect();
        if current.len() != ordered_ids.len()
            || ordered_set.len() != ordered_ids.len()
            || current_set != ordered_set
        {
            return Err(ClassificationError::invalid_structure(
                "ordered_category_ids must contain every sibling exactly once",
            ));
        }
        for (position, id) in ordered_ids.into_iter().enumerate() {
            let node = next.category_mut(id)?;
            node.position = position as i32;
            node.bump(now);
        }
        next.validate_command()?;
        next.bump(now);
        *self = next;
        Ok(())
    }

    pub(crate) fn set_lifecycle(
        &mut self,
        expected_version: i64,
        id: CategoryId,
        lifecycle: CategoryLifecycle,
        now: DateTime<Utc>,
    ) -> Result<(), ClassificationError> {
        self.category(id)?;
        self.require_version(expected_version)?;
        let mut next = self.clone();
        let node = next.category_mut(id)?;
        if node.lifecycle == lifecycle {
            return Err(ClassificationError::lifecycle_conflict());
        }
        node.lifecycle = lifecycle;
        node.bump(now);
        next.validate_command()?;
        next.bump(now);
        *self = next;
        Ok(())
    }

    pub(crate) fn category(&self, id: CategoryId) -> Result<&Category, ClassificationError> {
        self.categories
            .iter()
            .find(|category| category.id() == id)
            .ok_or_else(ClassificationError::not_found)
    }

    fn category_mut(&mut self, id: CategoryId) -> Result<&mut Category, ClassificationError> {
        self.categories
            .iter_mut()
            .find(|category| category.id() == id)
            .ok_or_else(ClassificationError::not_found)
    }

    pub(crate) fn children_ids(&self, parent_id: Option<CategoryId>) -> Vec<CategoryId> {
        let mut children: Vec<_> = self
            .categories
            .iter()
            .filter(|category| category.parent_id() == parent_id)
            .collect();
        children.sort_by_key(|category| (category.position(), category.id()));
        children.into_iter().map(Category::id).collect()
    }

    pub(crate) fn subtree_ids(&self, root: CategoryId) -> Vec<CategoryId> {
        let mut result = Vec::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if result.contains(&id) {
                continue;
            }
            result.push(id);
            pending.extend(self.children_ids(Some(id)).into_iter().rev());
        }
        result
    }

    pub(crate) fn depth(&self, id: CategoryId) -> Result<usize, ClassificationError> {
        let mut depth = 1;
        let mut cursor = self.category(id)?.parent_id();
        let mut seen = HashSet::from([id]);
        while let Some(parent_id) = cursor {
            if !seen.insert(parent_id) {
                return Err(ClassificationError::invalid_structure(
                    "category hierarchy contains a cycle",
                ));
            }
            depth += 1;
            cursor = self.category(parent_id)?.parent_id();
        }
        Ok(depth)
    }

    pub(crate) fn path(&self, id: CategoryId) -> Result<Vec<String>, ClassificationError> {
        let mut path = Vec::new();
        let mut cursor = Some(id);
        while let Some(category_id) = cursor {
            let category = self.category(category_id)?;
            path.push(category.name().to_owned());
            cursor = category.parent_id();
        }
        path.reverse();
        Ok(path)
    }

    pub(crate) fn is_effectively_active(
        &self,
        id: CategoryId,
    ) -> Result<bool, ClassificationError> {
        let mut cursor = Some(id);
        while let Some(category_id) = cursor {
            let category = self.category(category_id)?;
            if category.lifecycle() == CategoryLifecycle::Archived {
                return Ok(false);
            }
            cursor = category.parent_id();
        }
        Ok(true)
    }

    pub(crate) fn is_leaf(&self, id: CategoryId) -> bool {
        !self
            .categories
            .iter()
            .any(|category| category.parent_id() == Some(id))
    }

    pub(crate) fn effective_color(&self, id: CategoryId) -> Result<String, ClassificationError> {
        let mut cursor = Some(id);
        while let Some(category_id) = cursor {
            let category = self.category(category_id)?;
            if let Some(color) = category.color() {
                return Ok(color.to_owned());
            }
            cursor = category.parent_id();
        }
        Ok(DEFAULT_COLOR.to_owned())
    }

    pub(crate) fn effective_icon(&self, id: CategoryId) -> Result<String, ClassificationError> {
        let mut cursor = Some(id);
        while let Some(category_id) = cursor {
            let category = self.category(category_id)?;
            if let Some(icon) = category.icon() {
                return Ok(icon.to_owned());
            }
            cursor = category.parent_id();
        }
        Ok(DEFAULT_ICON.to_owned())
    }

    fn insert_at(&mut self, parent_id: Option<CategoryId>, id: CategoryId, at: usize) {
        let mut siblings: Vec<_> = self
            .children_ids(parent_id)
            .into_iter()
            .filter(|candidate| *candidate != id)
            .collect();
        siblings.insert(at, id);
        for (position, sibling) in siblings.into_iter().enumerate() {
            self.category_mut(sibling).expect("sibling exists").position = position as i32;
        }
    }

    fn remove_from_siblings(&mut self, parent_id: Option<CategoryId>, id: CategoryId) {
        let siblings: Vec<_> = self
            .children_ids(parent_id)
            .into_iter()
            .filter(|candidate| *candidate != id)
            .collect();
        for (position, sibling) in siblings.into_iter().enumerate() {
            self.category_mut(sibling).expect("sibling exists").position = position as i32;
        }
    }

    fn require_parent_accepts(
        &self,
        parent_id: Option<CategoryId>,
        child_kind: CategoryKind,
    ) -> Result<(), ClassificationError> {
        let Some(parent_id) = parent_id else {
            return Ok(());
        };
        let parent = self.category(parent_id)?;
        if !self.is_effectively_active(parent_id)? {
            return Err(ClassificationError::archived());
        }
        if parent.kind() != CategoryKind::Both && parent.kind() != child_kind {
            return Err(ClassificationError::invalid_structure(
                "parent category kind does not accept the child kind",
            ));
        }
        Ok(())
    }

    fn require_version(&self, expected_version: i64) -> Result<(), ClassificationError> {
        if expected_version != self.version {
            return Err(ClassificationError::version_conflict());
        }
        Ok(())
    }

    fn bump(&mut self, now: DateTime<Utc>) {
        self.version += 1;
        self.updated_at = now;
    }

    fn validate_command(&self) -> Result<(), ClassificationError> {
        self.validate().map_err(|message| match message {
            "duplicate active sibling category name" => {
                ClassificationError::duplicate_name_without_source()
            }
            other => ClassificationError::invalid_structure(other),
        })
    }

    fn validate_stored(&self) -> Result<(), ClassificationError> {
        self.validate().map_err(ClassificationError::persistence)
    }

    fn validate(&self) -> Result<(), &'static str> {
        let ids: HashSet<_> = self.categories.iter().map(Category::id).collect();
        if ids.len() != self.categories.len()
            || self
                .categories
                .iter()
                .any(|category| category.user_id() != self.user_id)
        {
            return Err("category taxonomy contains invalid ownership or duplicate ids");
        }
        let mut positions: HashMap<Option<CategoryId>, Vec<i32>> = HashMap::new();
        let mut names = HashSet::new();
        for category in &self.categories {
            if category
                .parent_id()
                .is_some_and(|parent| !ids.contains(&parent))
            {
                return Err("category parent was not found");
            }
            let depth = self
                .depth(category.id())
                .map_err(|_| "category hierarchy contains a cycle")?;
            if depth > MAX_DEPTH {
                return Err("category hierarchy cannot exceed three levels");
            }
            if let Some(parent_id) = category.parent_id() {
                let parent = self
                    .category(parent_id)
                    .map_err(|_| "category parent was not found")?;
                if parent.kind() != CategoryKind::Both && parent.kind() != category.kind() {
                    return Err("parent category kind does not accept the child kind");
                }
            }
            positions
                .entry(category.parent_id())
                .or_default()
                .push(category.position());
            if category.lifecycle() == CategoryLifecycle::Active
                && !names.insert((category.parent_id(), category.name().to_lowercase()))
            {
                return Err("duplicate active sibling category name");
            }
        }
        for mut values in positions.into_values() {
            values.sort_unstable();
            if values != (0..values.len() as i32).collect::<Vec<_>>() {
                return Err("category sibling positions are not dense");
            }
        }
        Ok(())
    }

    fn insert_starter_nodes(&mut self, now: DateTime<Utc>) {
        let income = self.template_node(
            None,
            0,
            "Income",
            CategoryKind::Income,
            Some("#16A34A"),
            Some("wallet-plus"),
            now,
        );
        for (position, (name, icon)) in [
            ("Salary", "briefcase"),
            ("Business & Freelance", "laptop"),
            ("Investments", "chart-line"),
            ("Other Income", "circle-ellipsis"),
        ]
        .into_iter()
        .enumerate()
        {
            self.template_node(
                Some(income),
                position as i32,
                name,
                CategoryKind::Income,
                None,
                Some(icon),
                now,
            );
        }

        let expenses = self.template_node(
            None,
            1,
            "Expenses",
            CategoryKind::Expense,
            Some("#DC2626"),
            Some("wallet-minus"),
            now,
        );
        let housing = self.template_node(
            Some(expenses),
            0,
            "Housing",
            CategoryKind::Expense,
            Some("#7C3AED"),
            Some("house"),
            now,
        );
        self.template_node(
            Some(housing),
            0,
            "Rent & Mortgage",
            CategoryKind::Expense,
            None,
            Some("building"),
            now,
        );
        self.template_node(
            Some(housing),
            1,
            "Utilities",
            CategoryKind::Expense,
            None,
            Some("bolt"),
            now,
        );
        let food = self.template_node(
            Some(expenses),
            1,
            "Food",
            CategoryKind::Expense,
            Some("#EA580C"),
            Some("utensils"),
            now,
        );
        self.template_node(
            Some(food),
            0,
            "Groceries",
            CategoryKind::Expense,
            None,
            Some("shopping-basket"),
            now,
        );
        self.template_node(
            Some(food),
            1,
            "Restaurants & Cafés",
            CategoryKind::Expense,
            None,
            Some("coffee"),
            now,
        );
        let transport = self.template_node(
            Some(expenses),
            2,
            "Transport",
            CategoryKind::Expense,
            Some("#2563EB"),
            Some("car"),
            now,
        );
        self.template_node(
            Some(transport),
            0,
            "Public Transport",
            CategoryKind::Expense,
            None,
            Some("bus"),
            now,
        );
        self.template_node(
            Some(transport),
            1,
            "Fuel",
            CategoryKind::Expense,
            None,
            Some("fuel"),
            now,
        );
        self.template_node(
            Some(transport),
            2,
            "Taxi & Ride Share",
            CategoryKind::Expense,
            None,
            Some("taxi"),
            now,
        );
        let health = self.template_node(
            Some(expenses),
            3,
            "Health",
            CategoryKind::Expense,
            Some("#DB2777"),
            Some("heart-pulse"),
            now,
        );
        self.template_node(
            Some(health),
            0,
            "Medical & Pharmacy",
            CategoryKind::Expense,
            None,
            Some("pill"),
            now,
        );
        for (offset, (name, icon)) in [
            ("Shopping", "shopping-bag"),
            ("Entertainment", "clapperboard"),
            ("Subscriptions", "repeat"),
            ("Travel", "plane"),
            ("Education", "graduation-cap"),
            ("Family & Gifts", "gift"),
            ("Fees & Taxes", "receipt"),
            ("Other Expenses", "circle-ellipsis"),
        ]
        .into_iter()
        .enumerate()
        {
            self.template_node(
                Some(expenses),
                (offset + 4) as i32,
                name,
                CategoryKind::Expense,
                None,
                Some(icon),
                now,
            );
        }
        debug_assert!(self.validate().is_ok());
    }

    #[allow(clippy::too_many_arguments)]
    fn template_node(
        &mut self,
        parent_id: Option<CategoryId>,
        position: i32,
        name: &str,
        kind: CategoryKind,
        color: Option<&str>,
        icon: Option<&str>,
        now: DateTime<Utc>,
    ) -> CategoryId {
        let id = CategoryId::generate();
        self.categories.push(
            Category::create(
                id,
                self.user_id,
                name.to_owned(),
                kind,
                parent_id,
                position,
                color.map(str::to_owned),
                icon.map(str::to_owned),
                now,
            )
            .expect("starter taxonomy is valid"),
        );
        id
    }

    pub(crate) fn user_id(&self) -> UserId {
        self.user_id
    }
    pub(crate) fn version(&self) -> i64 {
        self.version
    }
    pub(crate) fn starter_template_version(&self) -> Option<i32> {
        self.starter_template_version
    }
    pub(crate) fn categories(&self) -> &[Category] {
        &self.categories
    }
    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub(crate) fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

fn validate_name(name: String) -> Result<String, ClassificationError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ClassificationError::invalid_name());
    }
    Ok(name.to_owned())
}

fn validate_color(color: Option<String>) -> Result<Option<String>, ClassificationError> {
    let Some(color) = color else {
        return Ok(None);
    };
    let color = color.trim().to_ascii_uppercase();
    if color.len() != 7
        || !color.starts_with('#')
        || !color[1..].bytes().all(|value| value.is_ascii_hexdigit())
    {
        return Err(ClassificationError::invalid_style());
    }
    Ok(Some(color))
}

fn validate_icon(icon: Option<String>) -> Result<Option<String>, ClassificationError> {
    let Some(icon) = icon else {
        return Ok(None);
    };
    if !ICON_CATALOG.iter().any(|entry| entry.key == icon) {
        return Err(ClassificationError::invalid_style());
    }
    Ok(Some(icon))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone as _};
    use uuid::Uuid;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 4, 12, 0, 0).unwrap()
    }

    fn empty() -> CategoryTaxonomy {
        CategoryTaxonomy::for_existing(UserId::new(Uuid::from_u128(1)), Vec::new(), now()).unwrap()
    }

    fn create(
        taxonomy: &mut CategoryTaxonomy,
        name: &str,
        kind: CategoryKind,
        parent_id: Option<CategoryId>,
    ) -> CategoryId {
        taxonomy
            .create_node(
                taxonomy.version(),
                name.to_owned(),
                kind,
                parent_id,
                None,
                None,
                None,
                now() + Duration::seconds(taxonomy.version()),
            )
            .unwrap()
    }

    #[test]
    fn starter_taxonomy_matches_the_version_one_template() {
        let taxonomy = CategoryTaxonomy::starter(UserId::new(Uuid::from_u128(2)), now());
        assert_eq!(taxonomy.version(), 1);
        assert_eq!(taxonomy.starter_template_version(), Some(1));
        assert_eq!(taxonomy.categories().len(), 26);
        let roots = taxonomy.children_ids(None);
        assert_eq!(roots.len(), 2);
        assert_eq!(taxonomy.category(roots[0]).unwrap().name(), "Income");
        assert_eq!(taxonomy.category(roots[1]).unwrap().name(), "Expenses");
        assert_eq!(
            taxonomy
                .categories()
                .iter()
                .filter(|category| taxonomy.is_leaf(category.id()))
                .count(),
            20
        );
        assert!(taxonomy.validate().is_ok());
    }

    #[test]
    fn hierarchy_rejects_a_fourth_level_and_cycles_without_mutating() {
        let mut taxonomy = empty();
        let root = create(&mut taxonomy, "Root", CategoryKind::Expense, None);
        let child = create(&mut taxonomy, "Child", CategoryKind::Expense, Some(root));
        let leaf = create(&mut taxonomy, "Leaf", CategoryKind::Expense, Some(child));
        let before = taxonomy.clone();
        let depth_error = taxonomy
            .create_node(
                taxonomy.version(),
                "Too deep".to_owned(),
                CategoryKind::Expense,
                Some(leaf),
                None,
                None,
                None,
                now(),
            )
            .unwrap_err();
        assert!(depth_error.is_invalid_structure());
        assert_eq!(taxonomy.version(), before.version());
        assert_eq!(taxonomy.categories(), before.categories());

        let cycle_error = taxonomy
            .move_node(taxonomy.version(), root, Some(leaf), 0, now())
            .unwrap_err();
        assert!(cycle_error.is_invalid_structure());
        assert_eq!(taxonomy.categories(), before.categories());
    }

    #[test]
    fn parent_kind_contains_children_but_both_accepts_either_kind() {
        let mut taxonomy = empty();
        let expense = create(&mut taxonomy, "Expense", CategoryKind::Expense, None);
        let before = taxonomy.clone();
        let error = taxonomy
            .create_node(
                taxonomy.version(),
                "Salary".to_owned(),
                CategoryKind::Income,
                Some(expense),
                None,
                None,
                None,
                now(),
            )
            .unwrap_err();
        assert!(error.is_invalid_structure());
        assert_eq!(taxonomy.categories(), before.categories());

        let both = create(&mut taxonomy, "Everything", CategoryKind::Both, None);
        create(
            &mut taxonomy,
            "Business income",
            CategoryKind::Income,
            Some(both),
        );
        create(
            &mut taxonomy,
            "Business expense",
            CategoryKind::Expense,
            Some(both),
        );
        assert!(taxonomy.validate().is_ok());
    }

    #[test]
    fn active_sibling_names_are_case_insensitive_and_restore_can_conflict() {
        let mut taxonomy = empty();
        let original = create(&mut taxonomy, "Travel", CategoryKind::Expense, None);
        let duplicate = taxonomy
            .create_node(
                taxonomy.version(),
                " travel ".to_owned(),
                CategoryKind::Expense,
                None,
                None,
                None,
                None,
                now(),
            )
            .unwrap_err();
        assert!(duplicate.is_duplicate_name());

        taxonomy
            .set_lifecycle(
                taxonomy.version(),
                original,
                CategoryLifecycle::Archived,
                now(),
            )
            .unwrap();
        create(&mut taxonomy, "TRAVEL", CategoryKind::Expense, None);
        let before = taxonomy.clone();
        let restore = taxonomy
            .set_lifecycle(
                taxonomy.version(),
                original,
                CategoryLifecycle::Active,
                now(),
            )
            .unwrap_err();
        assert!(restore.is_duplicate_name());
        assert_eq!(taxonomy.categories(), before.categories());
    }

    #[test]
    fn reorder_requires_every_sibling_once_and_keeps_dense_positions() {
        let mut taxonomy = empty();
        let first = create(&mut taxonomy, "First", CategoryKind::Both, None);
        let second = create(&mut taxonomy, "Second", CategoryKind::Both, None);
        let third = create(&mut taxonomy, "Third", CategoryKind::Both, None);
        let before = taxonomy.clone();
        let error = taxonomy
            .reorder(taxonomy.version(), None, vec![first, first, third], now())
            .unwrap_err();
        assert!(error.is_invalid_structure());
        assert_eq!(taxonomy.categories(), before.categories());

        taxonomy
            .reorder(taxonomy.version(), None, vec![third, second, first], now())
            .unwrap();
        assert_eq!(taxonomy.children_ids(None), vec![third, second, first]);
        assert_eq!(
            taxonomy
                .children_ids(None)
                .into_iter()
                .map(|id| taxonomy.category(id).unwrap().position())
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn effective_archive_leaf_status_and_style_inheritance_are_derived() {
        let mut taxonomy = empty();
        let root = taxonomy
            .create_node(
                1,
                "Food".to_owned(),
                CategoryKind::Expense,
                None,
                None,
                Some("#ea580c".to_owned()),
                Some("utensils".to_owned()),
                now(),
            )
            .unwrap();
        let child = create(
            &mut taxonomy,
            "Groceries",
            CategoryKind::Expense,
            Some(root),
        );
        assert!(taxonomy.is_leaf(child));
        assert_eq!(taxonomy.effective_color(child).unwrap(), "#EA580C");
        assert_eq!(taxonomy.effective_icon(child).unwrap(), "utensils");

        taxonomy
            .set_lifecycle(taxonomy.version(), root, CategoryLifecycle::Archived, now())
            .unwrap();
        let archived_version = taxonomy.version();
        let duplicate_archive = taxonomy
            .set_lifecycle(archived_version, root, CategoryLifecycle::Archived, now())
            .unwrap_err();
        assert!(duplicate_archive.is_lifecycle_conflict());
        assert_eq!(taxonomy.version(), archived_version);
        assert!(!taxonomy.is_effectively_active(child).unwrap());
        assert_eq!(
            taxonomy.category(child).unwrap().lifecycle(),
            CategoryLifecycle::Active
        );
        taxonomy
            .set_lifecycle(taxonomy.version(), root, CategoryLifecycle::Active, now())
            .unwrap();
        let restored_version = taxonomy.version();
        assert!(
            taxonomy
                .set_lifecycle(restored_version, root, CategoryLifecycle::Active, now(),)
                .unwrap_err()
                .is_lifecycle_conflict()
        );
        assert_eq!(taxonomy.version(), restored_version);
        assert!(taxonomy.is_effectively_active(child).unwrap());

        create(
            &mut taxonomy,
            "Weekly shop",
            CategoryKind::Expense,
            Some(child),
        );
        assert!(!taxonomy.is_leaf(child));
        let fallback = create(&mut taxonomy, "No style", CategoryKind::Both, None);
        assert_eq!(taxonomy.effective_color(fallback).unwrap(), DEFAULT_COLOR);
        assert_eq!(taxonomy.effective_icon(fallback).unwrap(), DEFAULT_ICON);
    }

    #[test]
    fn style_and_taxonomy_version_validation_are_strict() {
        let mut taxonomy = empty();
        let before = taxonomy.clone();
        let color = taxonomy
            .create_node(
                1,
                "Bad color".to_owned(),
                CategoryKind::Both,
                None,
                None,
                Some("red".to_owned()),
                None,
                now(),
            )
            .unwrap_err();
        assert!(color.is_invalid_style());
        assert_eq!(taxonomy.categories(), before.categories());
        let icon = taxonomy
            .create_node(
                1,
                "Bad icon".to_owned(),
                CategoryKind::Both,
                None,
                None,
                None,
                Some("unknown".to_owned()),
                now(),
            )
            .unwrap_err();
        assert!(icon.is_invalid_style());

        let id = create(&mut taxonomy, "Valid", CategoryKind::Both, None);
        let stale = taxonomy
            .update_node(1, id, Some("Stale".to_owned()), None, None, now())
            .unwrap_err();
        assert!(stale.is_version_conflict());
        assert_eq!(taxonomy.category(id).unwrap().name(), "Valid");
    }
}
