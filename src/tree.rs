//! The tests the pane browses, grouped as assembly, then fixture, then test.

use std::collections::{BTreeMap, HashSet};
use std::iter;

use crate::unity::TestItem;

pub struct Tree {
    pub assemblies: Vec<AssemblyNode>,
}

pub struct AssemblyNode {
    pub name: String,
    pub fixtures: Vec<FixtureNode>,
}

pub struct FixtureNode {
    /// Everything before the test's own name; empty for a test outside any fixture.
    pub name: String,
    pub tests: Vec<TestNode>,
}

pub struct TestNode {
    pub full_name: String,
    pub name: String,
    pub explicit: bool,
}

/// One visible line of the tree, by position in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Assembly(usize),
    Fixture(usize, usize),
    Test(usize, usize, usize),
}

/// Which branches are open. Assemblies start open and fixtures closed, so a large suite opens
/// as one line per fixture.
#[derive(Default)]
pub struct Expansion {
    closed_assemblies: HashSet<String>,
    open_fixtures: HashSet<(String, String)>,
}

impl Expansion {
    pub fn is_assembly_open(&self, assembly: &str) -> bool {
        !self.closed_assemblies.contains(assembly)
    }

    pub fn is_fixture_open(&self, assembly: &str, fixture: &str) -> bool {
        self.open_fixtures.contains(&(assembly.to_owned(), fixture.to_owned()))
    }

    pub fn set_assembly(&mut self, assembly: &str, open: bool) {
        if open {
            self.closed_assemblies.remove(assembly);
        } else {
            self.closed_assemblies.insert(assembly.to_owned());
        }
    }

    pub fn set_fixture(&mut self, assembly: &str, fixture: &str, open: bool) {
        let key = (assembly.to_owned(), fixture.to_owned());
        if open {
            self.open_fixtures.insert(key);
        } else {
            self.open_fixtures.remove(&key);
        }
    }
}

/// Stands in for an assembly the listing didn't name.
const UNKNOWN_ASSEMBLY: &str = "(unknown assembly)";

impl Tree {
    /// Fixtures sort by name; tests keep the order Unity listed them in.
    pub fn build(items: &[TestItem]) -> Self {
        let mut grouped: BTreeMap<&str, BTreeMap<&str, Vec<TestNode>>> = BTreeMap::new();
        for item in items {
            let (fixture, name) = split_full_name(&item.full_name);
            let assembly = item.assembly.as_deref().unwrap_or(UNKNOWN_ASSEMBLY);
            grouped.entry(assembly).or_default().entry(fixture).or_default().push(TestNode {
                full_name: item.full_name.clone(),
                name: name.to_owned(),
                explicit: item.explicit,
            });
        }
        let assemblies = grouped
            .into_iter()
            .map(|(assembly, fixtures)| AssemblyNode {
                name: assembly.to_owned(),
                fixtures: fixtures
                    .into_iter()
                    .map(|(fixture, tests)| FixtureNode { name: fixture.to_owned(), tests })
                    .collect(),
            })
            .collect();
        Self { assemblies }
    }

    /// Every test, with the assembly it belongs to.
    pub fn tests(&self) -> impl Iterator<Item = (&str, &TestNode)> {
        self.assemblies.iter().flat_map(|assembly| {
            assembly.fixtures.iter().flat_map(move |fixture| {
                fixture.tests.iter().map(move |test| (assembly.name.as_str(), test))
            })
        })
    }

    /// The visible rows. A non-empty `filter` keeps only tests whose full name contains it,
    /// ignoring case, and shows every branch that holds one open.
    pub fn rows(&self, expansion: &Expansion, filter: &str) -> Vec<Row> {
        let needle = filter.to_lowercase();
        let filtering = !needle.is_empty();
        let matches =
            |test: &TestNode| !filtering || test.full_name.to_lowercase().contains(&needle);
        self.assemblies
            .iter()
            .enumerate()
            .flat_map(|(a, assembly)| {
                let fixture_rows: Vec<Row> = assembly
                    .fixtures
                    .iter()
                    .enumerate()
                    .flat_map(|(f, fixture)| {
                        let tests: Vec<usize> = (0..fixture.tests.len())
                            .filter(|&t| matches(&fixture.tests[t]))
                            .collect();
                        let open =
                            filtering || expansion.is_fixture_open(&assembly.name, &fixture.name);
                        let shown = if open { tests.clone() } else { Vec::new() };
                        (!tests.is_empty())
                            .then_some(Row::Fixture(a, f))
                            .into_iter()
                            .chain(shown.into_iter().map(move |t| Row::Test(a, f, t)))
                    })
                    .collect();
                let open = filtering || expansion.is_assembly_open(&assembly.name);
                let shown = if open { fixture_rows.clone() } else { Vec::new() };
                (!fixture_rows.is_empty()).then_some(Row::Assembly(a)).into_iter().chain(shown)
            })
            .collect()
    }

    pub fn assembly(&self, a: usize) -> &AssemblyNode {
        &self.assemblies[a]
    }

    pub fn fixture(&self, a: usize, f: usize) -> &FixtureNode {
        &self.assemblies[a].fixtures[f]
    }

    pub fn test(&self, a: usize, f: usize, t: usize) -> &TestNode {
        &self.assemblies[a].fixtures[f].tests[t]
    }

    /// The tests under `row`, itself included when it is one.
    pub fn tests_under(&self, row: Row) -> Vec<&TestNode> {
        match row {
            Row::Assembly(a) => {
                self.assembly(a).fixtures.iter().flat_map(|fixture| fixture.tests.iter()).collect()
            }
            Row::Fixture(a, f) => self.fixture(a, f).tests.iter().collect(),
            Row::Test(a, f, t) => iter::once(self.test(a, f, t)).collect(),
        }
    }
}

/// A test's full name split into its fixture and its own name, at the last dot outside
/// parentheses, so `Fixture.Case(1.5)` keeps `Case(1.5)` whole.
pub fn split_full_name(full_name: &str) -> (&str, &str) {
    let mut depth = 0usize;
    let mut split = None;
    for (index, ch) in full_name.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '.' if depth == 0 => split = Some(index),
            _ => {}
        }
    }
    match split {
        Some(index) => (&full_name[..index], &full_name[index + 1..]),
        None => ("", full_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(full_name: &str, assembly: &str) -> TestItem {
        TestItem { full_name: full_name.into(), assembly: Some(assembly.into()), explicit: false }
    }

    #[test]
    fn splits_at_the_last_dot_outside_parentheses() {
        assert_eq!(split_full_name("Ns.Fixture.Case"), ("Ns.Fixture", "Case"));
        assert_eq!(
            split_full_name("Ns.Fixture.Case(1.5,\"a.b\")"),
            ("Ns.Fixture", "Case(1.5,\"a.b\")")
        );
        assert_eq!(split_full_name("Ns.Fixture(2.0).Case"), ("Ns.Fixture(2.0)", "Case"));
        assert_eq!(split_full_name("Case"), ("", "Case"));
    }

    #[test]
    fn groups_by_assembly_then_fixture() {
        let tree = Tree::build(&[
            item("B.Fix.One", "Second"),
            item("A.Fix.Two", "First"),
            item("A.Fix.One", "First"),
        ]);
        assert_eq!(tree.assemblies[0].name, "First");
        let tests: Vec<&str> =
            tree.assemblies[0].fixtures[0].tests.iter().map(|test| test.name.as_str()).collect();
        assert_eq!(tests, ["Two", "One"]);
    }

    #[test]
    fn fixtures_start_closed_and_assemblies_open() {
        let tree = Tree::build(&[item("A.Fix.One", "Asm"), item("A.Fix.Two", "Asm")]);
        let mut expansion = Expansion::default();
        assert_eq!(tree.rows(&expansion, ""), [Row::Assembly(0), Row::Fixture(0, 0)]);
        expansion.set_fixture("Asm", "A.Fix", true);
        assert_eq!(tree.rows(&expansion, "").len(), 4);
    }

    #[test]
    fn a_filter_opens_every_branch_holding_a_match() {
        let tree = Tree::build(&[item("A.Fix.One", "Asm"), item("A.Other.Two", "Asm")]);
        let mut expansion = Expansion::default();
        expansion.set_assembly("Asm", false);
        assert_eq!(
            tree.rows(&expansion, "two"),
            [Row::Assembly(0), Row::Fixture(0, 1), Row::Test(0, 1, 0)]
        );
    }
}
