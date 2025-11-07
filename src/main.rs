use std::env;

use anyhow::Context;
use prettytable::{Table, row};
use rkv::{StoreOptions, Value};
use serde::Deserialize;

type Rkv = rkv::Rkv<rkv::backend::SafeModeEnvironment>;
type SingleStore = rkv::SingleStore<rkv::backend::SafeModeDatabase>;
type Reader<'t> = rkv::Reader<rkv::backend::SafeModeRoTransaction<'t>>;

trait ValueExt {
    fn as_json<T>(self) -> anyhow::Result<T>
    where
        for<'de> T: Deserialize<'de>;
}

impl<'t> ValueExt for Value<'t> {
    fn as_json<T>(self) -> anyhow::Result<T>
    where
        for<'de> T: Deserialize<'de>,
    {
        match self {
            rkv::Value::Json(j) => serde_json::from_str(j).map_err(Into::into),
            _ => anyhow::bail!("Unsupported type"),
        }
    }
}

trait SingleStoreExt {
    fn get_as_json<'r, T, R, K>(&self, reader: &'r R, k: K) -> anyhow::Result<Option<T>>
    where
        for<'de> T: Deserialize<'de>,
        R: rkv::Readable<'r, Database = rkv::backend::SafeModeDatabase>,
        K: AsRef<[u8]>;
}

impl SingleStoreExt for SingleStore {
    fn get_as_json<'r, T, R, K>(&self, reader: &'r R, k: K) -> anyhow::Result<Option<T>>
    where
        for<'de> T: Deserialize<'de>,
        R: rkv::Readable<'r, Database = rkv::backend::SafeModeDatabase>,
        K: AsRef<[u8]>,
    {
        self.get(reader, k)?.map(|v| v.as_json::<T>()).transpose()
    }
}

fn main() -> anyhow::Result<()> {
    let Some(db_dir) = env::args().nth(1) else {
        anyhow::bail!("Usage: dump-nimbus-db FILENAME");
    };
    let rkv = Rkv::with_capacity::<rkv::backend::SafeMode>(db_dir.as_ref(), 6)?;

    with_single(&rkv, "meta", dump_meta)?;
    with_single(&rkv, "enrollments", dump_enrollments)?;
    with_single(&rkv, "experiments", dump_experiments)?;
    with_single(&rkv, "updates", dump_updates)?;

    Ok(())
}

fn with_single<T>(
    rkv: &Rkv,
    name: &str,
    f: impl FnOnce(&SingleStore, &'_ Reader<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let store = rkv.open_single(name, StoreOptions::default())?;
    let reader = rkv.read()?;

    f(&store, &reader)
}

fn dump_meta(store: &SingleStore, reader: &Reader) -> anyhow::Result<()> {
    const DB_VERSION: &str = "db_version";
    const LEGACY_PARTICIPATION: &str = "user-opt-in";
    const EXPERIMENT_PARTICIPATION: &str = "user-opt-in-experiments";
    const ROLLOUT_PARTICIPATION: &str = "user-opt-in-rollouts";

    let db_version = store
        .get_as_json::<u16, _, _>(reader, DB_VERSION)
        .context("could not read database version")?
        .context("database version missing")?;

    let (experiment_participation, rollout_participation) = match db_version {
        1 | 2 => {
            let v = store
                .get_as_json::<bool, _, _>(reader, LEGACY_PARTICIPATION)?
                .context("opt-in key missing")?;
            (v, v)
        }

        3 => (
            store
                .get_as_json::<bool, _, _>(reader, EXPERIMENT_PARTICIPATION)?
                .context("experiment opt-in key missing")?,
            store
                .get_as_json::<bool, _, _>(reader, ROLLOUT_PARTICIPATION)?
                .context("rollout opt-in key missing")?,
        ),

        _ => anyhow::bail!("Unsupported database version {db_version}"),
    };

    println!("db_version:               {db_version}");
    println!("experiment participation: {experiment_participation}");
    println!("rollout participation:    {rollout_participation}");
    println!();

    Ok(())
}

#[derive(Debug, Deserialize)]
pub enum EnrollmentStatus {
    Enrolled {
        reason: String,
        branch: String,
    },
    NotEnrolled {
        reason: String,
    },
    Disqualified {
        reason: String,
        branch: String,
    },
    WasEnrolled {
        branch: String,
        experiment_ended_at: u64,
    },
    Error {
        reason: String,
    },
}

#[derive(Debug, Deserialize)]
struct Enrollment {
    slug: String,
    status: EnrollmentStatus,
}

fn dump_enrollments(store: &SingleStore, reader: &Reader) -> anyhow::Result<()> {
    let enrollments: Vec<_> = store
        .iter_start(reader)?
        .into_iter()
        .map(|entry| {
            entry
                .context("failed to iterate enrollments table")?
                .1
                .as_json::<Enrollment>()
        })
        .collect::<anyhow::Result<_>>()?;

    if enrollments.is_empty() {
        println!("No enrollments");
    } else {
        println!("Enrollments:");

        let mut table = Table::new();
        table.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
        table.set_titles(row!["Slug", "Enrollment Status"]);

        for Enrollment { slug, status } in &enrollments {
            table.add_row(row![slug, format!("{:?}", status),]);
        }

        table.printstd();
    }
    println!();

    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Experiment {
    slug: String,
    is_rollout: bool,
    is_enrollment_paused: bool,
    feature_ids: Vec<String>,
}

fn dump_experiments(store: &SingleStore, reader: &Reader) -> anyhow::Result<()> {
    let experiments: Vec<_> = store
        .iter_start(reader)?
        .into_iter()
        .map(|entry| {
            entry
                .context("failed to iterate experiments table")?
                .1
                .as_json::<Experiment>()
        })
        .collect::<anyhow::Result<_>>()?;

    if experiments.is_empty() {
        println!("No experiments");
    } else {
        println!("Experiments:");

        let mut table = Table::new();
        table.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);

        table.set_titles(row!["Slug", "R/E", "Paused?", "Feature IDs"]);

        for experiment in experiments {
            table.add_row(row![
                experiment.slug,
                if experiment.is_rollout { "R" } else { "E" },
                if experiment.is_enrollment_paused {
                    "Y"
                } else {
                    " "
                },
                experiment.feature_ids.join(","),
            ]);
        }

        table.printstd();
    }
    println!();

    Ok(())
}

fn dump_updates(store: &SingleStore, reader: &Reader) -> anyhow::Result<()> {
    let experiments: Vec<_> = store
        .iter_start(reader)?
        .into_iter()
        .map(|entry| {
            let (key, value) = entry.context("failed to iterate updates table")?;
            Ok((str::from_utf8(key)?, value.as_json::<serde_json::Value>()?))
        })
        .collect::<anyhow::Result<_>>()?;

    if experiments.is_empty() {
        println!("No experiments");
    } else {
        println!("Experiments:");
        for experiment in &experiments {
            println!("  {:?}", experiment);
        }
    }
    println!();

    Ok(())
}
