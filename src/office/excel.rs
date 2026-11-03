//! Checks that only Excel workbooks have.

use quick_xml::events::Event;

use super::common::targets;
use super::{Ctx, ns};
use crate::report::{CheckId, Error, Finding};

/// `tracked-changes` in a workbook: the revision logs and user list of a shared workbook.
pub fn shared_workbook_log(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let mut found = vec![];
    for (short, detail) in [
        ("revisionHeaders", "1 shared-workbook revision list"),
        ("revisionLog", "1 shared-workbook revision log"),
        ("usernames", "1 shared-workbook user list"),
    ] {
        for part in targets(pkg, short) {
            let mut user = None;
            if short == "usernames" {
                pkg.walk(part, |r, ev| {
                    if let Event::Start(e) | Event::Empty(e) = ev
                        && user.is_none()
                        && r.is(e, ns::S, "userInfo")
                    {
                        user = r.attr(e, "name")?.map(|n| n.into_owned());
                    }
                    Ok(())
                })?;
            }
            let f = Finding::new(
                CheckId::TrackedChanges,
                "shared_workbook_log",
                ctx.place(part),
                1,
                "parts",
                detail.into(),
            );
            found.push(f.with_value(ctx.values, user.as_deref()));
        }
    }
    Ok(found)
}
