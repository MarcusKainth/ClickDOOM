//! The screen melt's schedule: how far the wipe has advanced at each frame.
//!
//! The renderer takes `melt_step` per frame and draws the wipe at that
//! point. How many passes each frame advanced by is a property of the run
//! the reference emulator recorded. The counts are committed under
//! `driver/melt/` and streamed into `melt_schedule`, and SQL turns them
//! into the running total.

use clickdoom_native::sql::Statement;

/// A demo with no committed schedule.
#[derive(Debug, thiserror::Error)]
#[error(
    "no melt schedule for demo {demo}. driver/melt/ holds one file per demo, \
     named after its lump in lower case; add {demo} there with its provenance"
)]
pub struct UnknownDemo {
    pub demo: String,
}

/// The table the schedule loads into.
pub const TABLE: &str = "melt_schedule";

/// One demo's committed schedule.
struct Committed {
    /// The demo lump's name, in lower case.
    demo: &'static str,
    /// The file's whole text: a header row of column names, then one row
    /// per melt frame.
    tsv: &'static str,
}

/// Every committed schedule. `driver/melt/README.md` says where each came
/// from.
const SCHEDULES: [Committed; 1] = [Committed {
    demo: "demo3",
    tsv: include_str!("../../melt/demo3.tsv"),
}];

/// The statements that fill `melt_schedule` for `demo`.
///
/// The rows travel as the request body, and the running total is a window
/// function over them, so nothing in the driver adds anything up.
pub fn load_statements(db: &str, demo: &str) -> Result<Vec<Statement>, UnknownDemo> {
    let wanted = demo.to_ascii_lowercase();
    let tsv = SCHEDULES
        .iter()
        .find(|s| s.demo == wanted)
        .map(|s| s.tsv)
        .ok_or_else(|| UnknownDemo {
            demo: demo.to_owned(),
        })?;
    Ok(vec![Statement::data(
        format!(
            "INSERT INTO {db}.{TABLE} \
             SELECT frame, passes, \
             toUInt8(sum(passes) OVER (ORDER BY frame ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)) \
             FROM input('frame UInt32, passes UInt8') \
             SETTINGS input_format_tsv_skip_first_lines = 1 FORMAT TSV"
        ),
        tsv.as_bytes().to_vec(),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_committed_file_names_its_columns_and_holds_rows() {
        for schedule in &SCHEDULES {
            let mut lines = schedule.tsv.lines();
            assert_eq!(lines.next(), Some("frame\tpasses"), "{}", schedule.demo);
            let rows: Vec<&str> = lines.collect();
            assert!(!rows.is_empty(), "{} is empty", schedule.demo);
            for (at, row) in rows.iter().enumerate() {
                let (frame, passes) = row.split_once('\t').expect("two columns");
                assert_eq!(frame.parse::<u32>().ok(), Some(at as u32), "{row:?}");
                assert!(passes.parse::<u8>().is_ok_and(|p| p > 0), "{row:?}");
            }
        }
    }

    #[test]
    fn the_demo_name_is_matched_whatever_its_case() {
        assert!(load_statements("nat", "DEMO3").is_ok());
        assert!(load_statements("nat", "demo3").is_ok());
        let error = load_statements("nat", "DEMO1").unwrap_err();
        assert!(error.to_string().contains("DEMO1"), "{error}");
    }

    #[test]
    fn the_insert_streams_the_committed_text_past_its_header() {
        let statements = load_statements("nat", "demo3").unwrap();
        let insert = &statements[0];
        assert!(insert.sql.starts_with("INSERT INTO nat.melt_schedule"));
        assert!(insert.sql.contains("input_format_tsv_skip_first_lines = 1"));
        assert!(insert.body.starts_with(b"frame\tpasses\n"));
    }
}
