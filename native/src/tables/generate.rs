//! Turning the vendored engine's constant tables into TSV.
//!
//! One [`Table`] per C array. The column names are this file's, and the
//! struct-shaped ones are checked against the field names the header
//! declares, so a field added or reordered upstream is an error rather
//! than a column of values that quietly mean something else.

use std::collections::BTreeMap;
use std::path::Path;

use crate::csource::error::CError;
use crate::csource::init::{Array, Ctx, Node, check_struct, find_array};
use crate::csource::lex::{Tok, lex};
use crate::csource::symbols::Symbols;

/// The headers and sources every table is read from. Symbols are taken
/// from all of them at once, so a table may name a constant any of them
/// defines.
pub const SOURCES: [&str; 18] = [
    "doomtype.h",
    "m_fixed.h",
    "i_video.h",
    "tables.h",
    "doomdef.h",
    "sounds.h",
    "info.h",
    "p_mobj.h",
    "d_items.h",
    "p_spec.h",
    "info.c",
    "tables.c",
    "m_random.c",
    "d_items.c",
    "r_draw.c",
    "r_bsp.c",
    "p_spec.c",
    "p_switch.c",
];

/// One generated table.
pub struct Table {
    /// The file stem it is written under, and the ClickHouse table name.
    pub name: &'static str,
    /// The C file the values came from.
    pub source: &'static str,
    pub columns: Vec<&'static str>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// The table as TSV, one header row of column names then one row per
    /// entry. This is the exact text `native/tables/` holds.
    pub fn to_tsv(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.columns.join("\t"));
        out.push('\n');
        for row in &self.rows {
            out.push_str(&row.join("\t"));
            out.push('\n');
        }
        out
    }
}

/// Reads every table out of the vendored source at `dir`.
pub fn generate(dir: &Path) -> Result<Vec<Table>, CError> {
    let texts: BTreeMap<&str, String> = SOURCES
        .iter()
        .map(|name| Ok((*name, read(dir, name)?)))
        .collect::<Result<_, CError>>()?;
    let lexed: BTreeMap<&str, Vec<Tok<'_>>> = texts
        .iter()
        .map(|(name, text)| Ok((*name, lex(name, text)?)))
        .collect::<Result<_, CError>>()?;

    let mut symbols = Symbols::new();
    for (name, toks) in &lexed {
        symbols.absorb(name, toks);
    }
    symbols.resolve()?;

    let reader = Reader {
        lexed: &lexed,
        symbols: &symbols,
    };
    let mut tables = Vec::new();
    let mut actions = Vec::new();
    tables.push(reader.states(&mut actions)?);
    tables.push(action_functions(&actions));
    tables.push(reader.mobjinfo()?);
    tables.push(reader.sprnames()?);
    tables.push(reader.weaponinfo()?);
    tables.push(reader.animdefs()?);
    tables.push(reader.switch_list()?);
    tables.push(reader.checkcoord()?);
    for (name, source, array) in VALUE_TABLES {
        tables.push(reader.values(name, source, array)?);
    }
    tables.push(reader.gammatable()?);
    Ok(tables)
}

/// Generates every table from the source at `dir` and writes it to
/// `out/<name>.tsv`. Returns the file names written, in table order.
pub fn write_all(dir: &Path, out: &Path) -> Result<Vec<String>, CError> {
    std::fs::create_dir_all(out).map_err(|source| CError::Write {
        path: out.display().to_string(),
        source,
    })?;
    let mut written = Vec::new();
    for table in generate(dir)? {
        let name = format!("{}.tsv", table.name);
        let path = out.join(&name);
        std::fs::write(&path, table.to_tsv()).map_err(|source| CError::Write {
            path: path.display().to_string(),
            source,
        })?;
        written.push(name);
    }
    Ok(written)
}

fn read(dir: &Path, name: &str) -> Result<String, CError> {
    let path = dir.join(name);
    std::fs::read_to_string(&path).map_err(|source| CError::Read {
        path: path.display().to_string(),
        source,
    })
}

/// `(table name, C file, C array)` for the tables that are a flat list of
/// integers.
const VALUE_TABLES: [(&str, &str, &str); 5] = [
    ("finetangent", "tables.c", "finetangent"),
    ("finesine", "tables.c", "finesine"),
    ("tantoangle", "tables.c", "tantoangle"),
    ("rndtable", "m_random.c", "rndtable"),
    ("fuzzoffset", "r_draw.c", "fuzzoffset"),
];

/// The `state_t` fields, in the order `info.c` initializes them.
const STATE_FIELDS: [&str; 7] = [
    "sprite",
    "frame",
    "tics",
    "action",
    "nextstate",
    "misc1",
    "misc2",
];

/// The `mobjinfo_t` fields.
const MOBJ_FIELDS: [&str; 23] = [
    "doomednum",
    "spawnstate",
    "spawnhealth",
    "seestate",
    "seesound",
    "reactiontime",
    "attacksound",
    "painstate",
    "painchance",
    "painsound",
    "meleestate",
    "missilestate",
    "deathstate",
    "xdeathstate",
    "deathsound",
    "speed",
    "radius",
    "height",
    "mass",
    "damage",
    "activesound",
    "flags",
    "raisestate",
];

/// The `weaponinfo_t` fields.
const WEAPON_FIELDS: [&str; 6] = [
    "ammo",
    "upstate",
    "downstate",
    "readystate",
    "atkstate",
    "flashstate",
];

/// The `animdef_t` fields.
const ANIM_FIELDS: [&str; 4] = ["istexture", "endname", "startname", "speed"];

/// The `switchlist_t` fields.
const SWITCH_FIELDS: [&str; 3] = ["name1", "name2", "episode"];

struct Reader<'a> {
    lexed: &'a BTreeMap<&'static str, Vec<Tok<'a>>>,
    symbols: &'a Symbols<'a>,
}

impl<'a> Reader<'a> {
    fn ctx(&self, file: &'static str) -> Ctx<'a, 'a> {
        Ctx {
            file,
            symbols: self.symbols,
        }
    }

    fn toks(&self, file: &'static str) -> &'a [Tok<'a>] {
        self.lexed.get(file).map_or(&[], Vec::as_slice)
    }

    /// The array's entries, checked against its declared bound. A count
    /// that disagrees means the initializer and the declaration have
    /// drifted apart, and every index after the gap would be wrong.
    fn entries<'x>(
        &self,
        ctx: &Ctx<'_, 'a>,
        name: &str,
        array: &'x Array<'a>,
    ) -> Result<&'x [Node<'a>], CError> {
        let items = array.root.list(ctx)?;
        if let Some(Some(declared)) = array.bounds.first()
            && *declared != items.len() as i64
        {
            return Err(CError::TooManyEntries {
                file: ctx.file.to_owned(),
                name: name.to_owned(),
                declared: *declared,
                actual: items.len(),
            });
        }
        Ok(items)
    }

    fn array(&self, file: &'static str, name: &str) -> Result<(Ctx<'a, 'a>, Array<'a>), CError> {
        let ctx = self.ctx(file);
        let array = find_array(&ctx, self.toks(file), name)?;
        Ok((ctx, array))
    }

    /// `states[]`, with the action function names collected into
    /// `actions` so [`action_functions`] can number them.
    fn states(&self, actions: &mut Vec<String>) -> Result<Table, CError> {
        check_struct(
            &self.ctx("info.h"),
            self.toks("info.h"),
            "state_t",
            &STATE_FIELDS,
        )?;
        let (ctx, array) = self.array("info.c", "states")?;
        let items = self.entries(&ctx, "states", &array)?;

        let mut names: Vec<&str> = Vec::new();
        for state in items {
            names.push(state.list(&ctx)?[3].name(&ctx)?);
        }
        let mut sorted: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.retain(|n| n != "NULL");
        *actions = sorted;

        let mut rows = Vec::with_capacity(items.len());
        for (index, state) in items.iter().enumerate() {
            let fields = state.list(&ctx)?;
            let action = action_id(actions, names[index]);
            rows.push(vec![
                index.to_string(),
                fields[0].int(&ctx)?.to_string(),
                fields[1].int(&ctx)?.to_string(),
                fields[2].int(&ctx)?.to_string(),
                action.to_string(),
                fields[4].int(&ctx)?.to_string(),
                fields[5].int(&ctx)?.to_string(),
                fields[6].int(&ctx)?.to_string(),
            ]);
        }
        Ok(Table {
            name: "states",
            source: "info.c",
            columns: with_id(&STATE_FIELDS),
            rows,
        })
    }

    fn mobjinfo(&self) -> Result<Table, CError> {
        check_struct(
            &self.ctx("info.h"),
            self.toks("info.h"),
            "mobjinfo_t",
            &MOBJ_FIELDS,
        )?;
        self.struct_table("mobjinfo", "info.c", "mobjinfo", &MOBJ_FIELDS)
    }

    fn weaponinfo(&self) -> Result<Table, CError> {
        check_struct(
            &self.ctx("d_items.h"),
            self.toks("d_items.h"),
            "weaponinfo_t",
            &WEAPON_FIELDS,
        )?;
        self.struct_table("weaponinfo", "d_items.c", "weaponinfo", &WEAPON_FIELDS)
    }

    /// A table whose every field is an integer.
    fn struct_table(
        &self,
        name: &'static str,
        file: &'static str,
        array_name: &str,
        fields: &'static [&'static str],
    ) -> Result<Table, CError> {
        let (ctx, array) = self.array(file, array_name)?;
        let items = self.entries(&ctx, array_name, &array)?;
        let mut rows = Vec::with_capacity(items.len());
        for (index, entry) in items.iter().enumerate() {
            let mut row = vec![index.to_string()];
            for value in entry.ints(&ctx, fields.len())? {
                row.push(value.to_string());
            }
            rows.push(row);
        }
        Ok(Table {
            name,
            source: file,
            columns: with_id(fields),
            rows,
        })
    }

    /// `sprnames[]`, whose last entry is the `NULL` terminator rather
    /// than a name.
    fn sprnames(&self) -> Result<Table, CError> {
        let (ctx, array) = self.array("info.c", "sprnames")?;
        let items = array.root.list(&ctx)?;
        let mut rows = Vec::with_capacity(items.len());
        for (index, entry) in items.iter().enumerate() {
            if entry.name(&ctx).is_ok_and(|name| name == "NULL") {
                break;
            }
            rows.push(vec![index.to_string(), text(&ctx, entry)?]);
        }
        // The sprite numbers `states` holds index this table, so a name
        // list shorter than the enumeration reads past its end.
        let declared = self.symbols.get("NUMSPRITES").unwrap_or_default();
        if declared != rows.len() as i64 {
            return Err(CError::TooManyEntries {
                file: "info.c".to_owned(),
                name: "sprnames".to_owned(),
                declared,
                actual: rows.len(),
            });
        }
        Ok(Table {
            name: "sprnames",
            source: "info.c",
            columns: vec!["id", "name"],
            rows,
        })
    }

    /// `animdefs[]`, including the `istexture = -1` terminator, so the
    /// table is the array the engine reads.
    fn animdefs(&self) -> Result<Table, CError> {
        check_struct(
            &self.ctx("p_spec.c"),
            self.toks("p_spec.c"),
            "animdef_t",
            &ANIM_FIELDS,
        )?;
        let (ctx, array) = self.array("p_spec.c", "animdefs")?;
        let mut rows = Vec::new();
        for (index, entry) in array.root.list(&ctx)?.iter().enumerate() {
            let fields = entry.list(&ctx)?;
            rows.push(vec![
                index.to_string(),
                fields[0].int(&ctx)?.to_string(),
                text(&ctx, &fields[1])?,
                text(&ctx, &fields[2])?,
                fields[3].int(&ctx)?.to_string(),
            ]);
        }
        Ok(Table {
            name: "animdefs",
            source: "p_spec.c",
            columns: with_id(&ANIM_FIELDS),
            rows,
        })
    }

    /// `alphSwitchList[]`, including its empty-name terminator.
    fn switch_list(&self) -> Result<Table, CError> {
        check_struct(
            &self.ctx("p_spec.h"),
            self.toks("p_spec.h"),
            "switchlist_t",
            &SWITCH_FIELDS,
        )?;
        let (ctx, array) = self.array("p_switch.c", "alphSwitchList")?;
        let mut rows = Vec::new();
        for (index, entry) in array.root.list(&ctx)?.iter().enumerate() {
            let fields = entry.list(&ctx)?;
            rows.push(vec![
                index.to_string(),
                text(&ctx, &fields[0])?,
                text(&ctx, &fields[1])?,
                fields[2].int(&ctx)?.to_string(),
            ]);
        }
        Ok(Table {
            name: "switchlist",
            source: "p_switch.c",
            columns: with_id(&SWITCH_FIELDS),
            rows,
        })
    }

    /// `checkcoord[12][4]`, whose last row and two of its middle rows are
    /// partial initializers C fills with zeros.
    fn checkcoord(&self) -> Result<Table, CError> {
        let (ctx, array) = self.array("r_bsp.c", "checkcoord")?;
        let items = array.root.list(&ctx)?;
        let mut rows = Vec::new();
        for (index, entry) in items.iter().enumerate() {
            let mut row = vec![index.to_string()];
            row.extend(entry.ints(&ctx, 4)?.iter().map(i64::to_string));
            rows.push(row);
        }
        if let Some(Some(declared)) = array.bounds.first() {
            while (rows.len() as i64) < *declared {
                let index = rows.len().to_string();
                rows.push(vec![index, "0".into(), "0".into(), "0".into(), "0".into()]);
            }
        }
        Ok(Table {
            name: "checkcoord",
            source: "r_bsp.c",
            columns: vec!["id", "c0", "c1", "c2", "c3"],
            rows,
        })
    }

    /// A flat array of integers, as `(id, value)` rows.
    fn values(
        &self,
        name: &'static str,
        file: &'static str,
        array_name: &str,
    ) -> Result<Table, CError> {
        let (ctx, array) = self.array(file, array_name)?;
        let items = self.entries(&ctx, array_name, &array)?;
        let mut rows = Vec::with_capacity(items.len());
        for (index, entry) in items.iter().enumerate() {
            rows.push(vec![index.to_string(), entry.int(&ctx)?.to_string()]);
        }
        Ok(Table {
            name,
            source: file,
            columns: vec!["id", "value"],
            rows,
        })
    }

    /// `gammatable[5][256]`, as `(level, id, value)` rows.
    fn gammatable(&self) -> Result<Table, CError> {
        let (ctx, array) = self.array("tables.c", "gammatable")?;
        let levels = self.entries(&ctx, "gammatable", &array)?;
        let width = match array.bounds.get(1) {
            Some(Some(width)) => *width as usize,
            _ => 0,
        };
        let mut rows = Vec::with_capacity(levels.len() * width);
        for (level, entry) in levels.iter().enumerate() {
            for (index, value) in entry.ints(&ctx, width)?.iter().enumerate() {
                rows.push(vec![
                    level.to_string(),
                    index.to_string(),
                    value.to_string(),
                ]);
            }
        }
        Ok(Table {
            name: "gammatable",
            source: "tables.c",
            columns: vec!["level", "id", "value"],
            rows,
        })
    }
}

/// The action functions `states` names, numbered from 1. Zero is `NULL`,
/// the state that runs nothing.
fn action_functions(actions: &[String]) -> Table {
    let mut rows = vec![vec!["0".to_owned(), "NULL".to_owned()]];
    for (index, name) in actions.iter().enumerate() {
        rows.push(vec![(index + 1).to_string(), name.clone()]);
    }
    Table {
        name: "action_functions",
        source: "info.c",
        columns: vec!["id", "name"],
        rows,
    }
}

fn action_id(actions: &[String], name: &str) -> usize {
    match actions.iter().position(|a| a == name) {
        Some(at) => at + 1,
        None => 0,
    }
}

fn with_id(fields: &[&'static str]) -> Vec<&'static str> {
    std::iter::once("id")
        .chain(fields.iter().copied())
        .collect()
}

/// A string leaf, checked to hold nothing a TSV cell cannot carry.
fn text<'a>(ctx: &Ctx<'_, 'a>, node: &Node<'a>) -> Result<String, CError> {
    let text = node.text(ctx)?;
    if text.contains(['\t', '\n', '\r']) {
        return Err(CError::Expected {
            file: ctx.file.to_owned(),
            line: node.line(),
            want: "a name without whitespace",
            found: format!("{text:?}"),
        });
    }
    Ok(text.to_owned())
}
