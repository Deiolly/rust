use crate::infer::canonical::query_response;
use crate::infer::{InferCtxt, InferOk};
use crate::traits::engine::TraitEngineExt as _;
use crate::traits::query::type_op::TypeOpOutput;
use crate::traits::query::Fallible;
use crate::traits::TraitEngine;
use rustc_infer::infer::region_constraints::RegionConstraintData;
use rustc_infer::traits::TraitEngineExt as _;
use rustc_span::source_map::DUMMY_SP;

use std::fmt;
use std::rc::Rc;

pub struct CustomTypeOp<F, G> {
    closure: F,
    description: G,
}

impl<F, G> CustomTypeOp<F, G> {
    pub fn new<'tcx, R>(closure: F, description: G) -> Self
    where
        F: FnOnce(&InferCtxt<'_, 'tcx>) -> Fallible<InferOk<'tcx, R>>,
        G: Fn() -> String,
    {
        CustomTypeOp { closure, description }
    }
}

impl<'tcx, F, R, G> super::TypeOp<'tcx> for CustomTypeOp<F, G>
where
    F: for<'a, 'cx> FnOnce(&'a InferCtxt<'cx, 'tcx>) -> Fallible<InferOk<'tcx, R>>,
    G: Fn() -> String,
{
    type Output = R;
    /// We can't do any custom error reporting for `CustomTypeOp`, so
    /// we can use `!` to enforce that the implementation never provides it.
    type ErrorInfo = !;

    /// Processes the operation and all resulting obligations,
    /// returning the final result along with any region constraints
    /// (they will be given over to the NLL region solver).
    fn fully_perform(self, infcx: &InferCtxt<'_, 'tcx>) -> Fallible<TypeOpOutput<'tcx, Self>> {
        if cfg!(debug_assertions) {
            info!("fully_perform({:?})", self);
        }

        Ok(scrape_region_constraints(infcx, || (self.closure)(infcx))?.0)
    }
}

impl<F, G> fmt::Debug for CustomTypeOp<F, G>
where
    G: Fn() -> String,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", (self.description)())
    }
}

/// Executes `op` and then scrapes out all the "old style" region
/// constraints that result, creating query-region-constraints.
pub fn scrape_region_constraints<'tcx, Op: super::TypeOp<'tcx, Output = R>, R>(
    infcx: &InferCtxt<'_, 'tcx>,
    op: impl FnOnce() -> Fallible<InferOk<'tcx, R>>,
) -> Fallible<(TypeOpOutput<'tcx, Op>, RegionConstraintData<'tcx>)> {
    let mut fulfill_cx = <dyn TraitEngine<'_>>::new(infcx.tcx);

    // During NLL, we expect that nobody will register region
    // obligations **except** as part of a custom type op (and, at the
    // end of each custom type op, we scrape out the region
    // obligations that resulted). So this vector should be empty on
    // entry.
    let pre_obligations = infcx.take_registered_region_obligations();
    assert!(
        pre_obligations.is_empty(),
        "scrape_region_constraints: incoming region obligations = {:#?}",
        pre_obligations,
    );

    let InferOk { value, obligations } = infcx.commit_if_ok(|_| op())?;
    fulfill_cx.register_predicate_obligations(infcx, obligations);
    let errors = fulfill_cx.select_all_or_error(infcx);
    if !errors.is_empty() {
        infcx.tcx.sess.diagnostic().delay_span_bug(
            DUMMY_SP,
            &format!("errors selecting obligation during MIR typeck: {:?}", errors),
        );
    }

    let region_obligations = infcx.take_registered_region_obligations();

    let region_constraint_data = infcx.take_and_reset_region_constraints();

    let region_constraints = query_response::make_query_region_constraints(
        infcx.tcx,
        region_obligations
            .iter()
            .map(|r_o| (r_o.sup_type, r_o.sub_region))
            .map(|(ty, r)| (infcx.resolve_vars_if_possible(ty), r)),
        &region_constraint_data,
    );

    if region_constraints.is_empty() {
        Ok((
            TypeOpOutput { output: value, constraints: None, error_info: None },
            region_constraint_data,
        ))
    } else {
        Ok((
            TypeOpOutput {
                output: value,
                constraints: Some(Rc::new(region_constraints)),
                error_info: None,
            },
            region_constraint_data,
        ))
    }
}
