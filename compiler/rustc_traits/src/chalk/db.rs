//! Provides the `RustIrDatabase` implementation for `chalk-solve`
//!
//! The purpose of the `chalk_solve::RustIrDatabase` is to get data about
//! specific types, such as bounds, where clauses, or fields. This file contains
//! the minimal logic to assemble the types for `chalk-solve` by calling out to
//! either the `TyCtxt` (for information about types) or
//! `crate::chalk::lowering` (to lower rustc types into Chalk types).

use rustc_middle::traits::ChalkRustInterner as RustInterner;
use rustc_middle::ty::subst::{InternalSubsts, Subst, SubstsRef};
use rustc_middle::ty::{self, AssocItemContainer, AssocKind, TyCtxt, TypeFoldable};

use rustc_hir::def_id::DefId;
use rustc_hir::Unsafety;

use rustc_span::symbol::sym;

use std::fmt;
use std::sync::Arc;

use crate::chalk::lowering::{self, LowerInto};

pub struct RustIrDatabase<'tcx> {
    pub(crate) interner: RustInterner<'tcx>,
    pub(crate) restatic_placeholder: ty::Region<'tcx>,
    pub(crate) reempty_placeholder: ty::Region<'tcx>,
}

impl fmt::Debug for RustIrDatabase<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RustIrDatabase")
    }
}

impl<'tcx> RustIrDatabase<'tcx> {
    fn where_clauses_for(
        &self,
        def_id: DefId,
        bound_vars: SubstsRef<'tcx>,
    ) -> Vec<chalk_ir::QuantifiedWhereClause<RustInterner<'tcx>>> {
        let predicates = self.interner.tcx.predicates_of(def_id).predicates;
        let mut regions_substitutor = lowering::RegionsSubstitutor::new(
            self.interner.tcx,
            self.restatic_placeholder,
            self.reempty_placeholder,
        );
        predicates
            .iter()
            .map(|(wc, _)| wc.subst(self.interner.tcx, bound_vars))
            .map(|wc| wc.fold_with(&mut regions_substitutor))
            .filter_map(|wc| LowerInto::<Option<chalk_ir::QuantifiedWhereClause<RustInterner<'tcx>>>>::lower_into(wc, &self.interner)).collect()
    }
}

impl<'tcx> chalk_solve::RustIrDatabase<RustInterner<'tcx>> for RustIrDatabase<'tcx> {
    fn interner(&self) -> &RustInterner<'tcx> {
        &self.interner
    }

    fn associated_ty_data(
        &self,
        assoc_type_id: chalk_ir::AssocTypeId<RustInterner<'tcx>>,
    ) -> Arc<chalk_solve::rust_ir::AssociatedTyDatum<RustInterner<'tcx>>> {
        let def_id = assoc_type_id.0;
        let assoc_item = self.interner.tcx.associated_item(def_id);
        let trait_def_id = match assoc_item.container {
            AssocItemContainer::TraitContainer(def_id) => def_id,
            _ => unimplemented!("Not possible??"),
        };
        match assoc_item.kind {
            AssocKind::Type => {}
            _ => unimplemented!("Not possible??"),
        }
        let bound_vars = bound_vars_for_item(self.interner.tcx, def_id);
        let binders = binders_for(&self.interner, bound_vars);
        // FIXME(chalk): this really isn't right I don't think. The functions
        // for GATs are a bit hard to figure out. Are these supposed to be where
        // clauses or bounds?
        let where_clauses = self.where_clauses_for(def_id, bound_vars);

        Arc::new(chalk_solve::rust_ir::AssociatedTyDatum {
            trait_id: chalk_ir::TraitId(trait_def_id),
            id: assoc_type_id,
            name: (),
            binders: chalk_ir::Binders::new(
                binders,
                chalk_solve::rust_ir::AssociatedTyDatumBound { bounds: vec![], where_clauses },
            ),
        })
    }

    fn trait_datum(
        &self,
        trait_id: chalk_ir::TraitId<RustInterner<'tcx>>,
    ) -> Arc<chalk_solve::rust_ir::TraitDatum<RustInterner<'tcx>>> {
        let def_id = trait_id.0;
        let trait_def = self.interner.tcx.trait_def(def_id);

        let bound_vars = bound_vars_for_item(self.interner.tcx, def_id);
        let binders = binders_for(&self.interner, bound_vars);

        let where_clauses = self.where_clauses_for(def_id, bound_vars);

        let associated_ty_ids: Vec<_> = self
            .interner
            .tcx
            .associated_items(def_id)
            .in_definition_order()
            .filter(|i| i.kind == AssocKind::Type)
            .map(|i| chalk_ir::AssocTypeId(i.def_id))
            .collect();

        let well_known = if self.interner.tcx.lang_items().sized_trait() == Some(def_id) {
            Some(chalk_solve::rust_ir::WellKnownTrait::Sized)
        } else if self.interner.tcx.lang_items().copy_trait() == Some(def_id) {
            Some(chalk_solve::rust_ir::WellKnownTrait::Copy)
        } else if self.interner.tcx.lang_items().clone_trait() == Some(def_id) {
            Some(chalk_solve::rust_ir::WellKnownTrait::Clone)
        } else if self.interner.tcx.lang_items().drop_trait() == Some(def_id) {
            Some(chalk_solve::rust_ir::WellKnownTrait::Drop)
        } else if self.interner.tcx.lang_items().fn_trait() == Some(def_id) {
            Some(chalk_solve::rust_ir::WellKnownTrait::Fn)
        } else if self
            .interner
            .tcx
            .lang_items()
            .fn_once_trait()
            .map(|t| def_id == t)
            .unwrap_or(false)
        {
            Some(chalk_solve::rust_ir::WellKnownTrait::FnOnce)
        } else if self
            .interner
            .tcx
            .lang_items()
            .fn_mut_trait()
            .map(|t| def_id == t)
            .unwrap_or(false)
        {
            Some(chalk_solve::rust_ir::WellKnownTrait::FnMut)
        } else {
            None
        };
        Arc::new(chalk_solve::rust_ir::TraitDatum {
            id: trait_id,
            binders: chalk_ir::Binders::new(
                binders,
                chalk_solve::rust_ir::TraitDatumBound { where_clauses },
            ),
            flags: chalk_solve::rust_ir::TraitFlags {
                auto: trait_def.has_auto_impl,
                marker: trait_def.is_marker,
                upstream: !def_id.is_local(),
                fundamental: self.interner.tcx.has_attr(def_id, sym::fundamental),
                non_enumerable: true,
                coinductive: false,
            },
            associated_ty_ids,
            well_known,
        })
    }

    fn adt_datum(
        &self,
        adt_id: chalk_ir::AdtId<RustInterner<'tcx>>,
    ) -> Arc<chalk_solve::rust_ir::AdtDatum<RustInterner<'tcx>>> {
        let adt_def = adt_id.0;

        let bound_vars = bound_vars_for_item(self.interner.tcx, adt_def.did);
        let binders = binders_for(&self.interner, bound_vars);

        let where_clauses = self.where_clauses_for(adt_def.did, bound_vars);

        let variants: Vec<_> = adt_def
            .variants
            .iter()
            .map(|variant| chalk_solve::rust_ir::AdtVariantDatum {
                fields: variant
                    .fields
                    .iter()
                    .map(|field| field.ty(self.interner.tcx, bound_vars).lower_into(&self.interner))
                    .collect(),
            })
            .collect();
        Arc::new(chalk_solve::rust_ir::AdtDatum {
            id: adt_id,
            binders: chalk_ir::Binders::new(
                binders,
                chalk_solve::rust_ir::AdtDatumBound { variants, where_clauses },
            ),
            flags: chalk_solve::rust_ir::AdtFlags {
                upstream: !adt_def.did.is_local(),
                fundamental: adt_def.is_fundamental(),
                phantom_data: adt_def.is_phantom_data(),
            },
            kind: match adt_def.adt_kind() {
                ty::AdtKind::Struct => chalk_solve::rust_ir::AdtKind::Struct,
                ty::AdtKind::Union => chalk_solve::rust_ir::AdtKind::Union,
                ty::AdtKind::Enum => chalk_solve::rust_ir::AdtKind::Enum,
            },
        })
    }

    fn adt_repr(
        &self,
        adt_id: chalk_ir::AdtId<RustInterner<'tcx>>,
    ) -> chalk_solve::rust_ir::AdtRepr {
        let adt_def = adt_id.0;
        chalk_solve::rust_ir::AdtRepr {
            repr_c: adt_def.repr.c(),
            repr_packed: adt_def.repr.packed(),
        }
    }

    fn fn_def_datum(
        &self,
        fn_def_id: chalk_ir::FnDefId<RustInterner<'tcx>>,
    ) -> Arc<chalk_solve::rust_ir::FnDefDatum<RustInterner<'tcx>>> {
        let def_id = fn_def_id.0;
        let bound_vars = bound_vars_for_item(self.interner.tcx, def_id);
        let binders = binders_for(&self.interner, bound_vars);

        let where_clauses = self.where_clauses_for(def_id, bound_vars);

        let sig = self.interner.tcx.fn_sig(def_id);
        let (inputs_and_output, iobinders, _) = crate::chalk::lowering::collect_bound_vars(
            &self.interner,
            self.interner.tcx,
            &sig.inputs_and_output().subst(self.interner.tcx, bound_vars),
        );

        let argument_types = inputs_and_output[..inputs_and_output.len() - 1]
            .iter()
            .map(|t| t.subst(self.interner.tcx, &bound_vars).lower_into(&self.interner))
            .collect();

        let return_type = inputs_and_output[inputs_and_output.len() - 1]
            .subst(self.interner.tcx, &bound_vars)
            .lower_into(&self.interner);

        let bound = chalk_solve::rust_ir::FnDefDatumBound {
            inputs_and_output: chalk_ir::Binders::new(
                iobinders,
                chalk_solve::rust_ir::FnDefInputsAndOutputDatum { argument_types, return_type },
            ),
            where_clauses,
        };
        Arc::new(chalk_solve::rust_ir::FnDefDatum {
            id: fn_def_id,
            abi: sig.abi(),
            safety: match sig.unsafety() {
                Unsafety::Normal => chalk_ir::Safety::Safe,
                Unsafety::Unsafe => chalk_ir::Safety::Unsafe,
            },
            variadic: sig.c_variadic(),
            binders: chalk_ir::Binders::new(binders, bound),
        })
    }

    fn impl_datum(
        &self,
        impl_id: chalk_ir::ImplId<RustInterner<'tcx>>,
    ) -> Arc<chalk_solve::rust_ir::ImplDatum<RustInterner<'tcx>>> {
        let def_id = impl_id.0;
        let bound_vars = bound_vars_for_item(self.interner.tcx, def_id);
        let binders = binders_for(&self.interner, bound_vars);

        let trait_ref = self.interner.tcx.impl_trait_ref(def_id).expect("not an impl");
        let trait_ref = trait_ref.subst(self.interner.tcx, bound_vars);
        let mut regions_substitutor = lowering::RegionsSubstitutor::new(
            self.interner.tcx,
            self.restatic_placeholder,
            self.reempty_placeholder,
        );
        let trait_ref = trait_ref.fold_with(&mut regions_substitutor);

        let where_clauses = self.where_clauses_for(def_id, bound_vars);

        let value = chalk_solve::rust_ir::ImplDatumBound {
            trait_ref: trait_ref.lower_into(&self.interner),
            where_clauses,
        };

        Arc::new(chalk_solve::rust_ir::ImplDatum {
            polarity: chalk_solve::rust_ir::Polarity::Positive,
            binders: chalk_ir::Binders::new(binders, value),
            impl_type: chalk_solve::rust_ir::ImplType::Local,
            associated_ty_value_ids: vec![],
        })
    }

    fn impls_for_trait(
        &self,
        trait_id: chalk_ir::TraitId<RustInterner<'tcx>>,
        parameters: &[chalk_ir::GenericArg<RustInterner<'tcx>>],
        _binders: &chalk_ir::CanonicalVarKinds<RustInterner<'tcx>>,
    ) -> Vec<chalk_ir::ImplId<RustInterner<'tcx>>> {
        let def_id = trait_id.0;

        // FIXME(chalk): use TraitDef::for_each_relevant_impl, but that will
        // require us to be able to interconvert `Ty<'tcx>`, and we're
        // not there yet.

        let all_impls = self.interner.tcx.all_impls(def_id);
        let matched_impls = all_impls.filter(|impl_def_id| {
            use chalk_ir::could_match::CouldMatch;
            let trait_ref = self.interner.tcx.impl_trait_ref(*impl_def_id).unwrap();
            let bound_vars = bound_vars_for_item(self.interner.tcx, *impl_def_id);

            let self_ty = trait_ref.self_ty();
            let self_ty = self_ty.subst(self.interner.tcx, bound_vars);
            let mut regions_substitutor = lowering::RegionsSubstitutor::new(
                self.interner.tcx,
                self.restatic_placeholder,
                self.reempty_placeholder,
            );
            let self_ty = self_ty.fold_with(&mut regions_substitutor);
            let lowered_ty = self_ty.lower_into(&self.interner);

            parameters[0].assert_ty_ref(&self.interner).could_match(&self.interner, &lowered_ty)
        });

        let impls = matched_impls.map(chalk_ir::ImplId).collect();
        impls
    }

    fn impl_provided_for(
        &self,
        auto_trait_id: chalk_ir::TraitId<RustInterner<'tcx>>,
        adt_id: chalk_ir::AdtId<RustInterner<'tcx>>,
    ) -> bool {
        let trait_def_id = auto_trait_id.0;
        let adt_def = adt_id.0;
        let all_impls = self.interner.tcx.all_impls(trait_def_id);
        for impl_def_id in all_impls {
            let trait_ref = self.interner.tcx.impl_trait_ref(impl_def_id).unwrap();
            let self_ty = trait_ref.self_ty();
            match *self_ty.kind() {
                ty::Adt(impl_adt_def, _) => {
                    if impl_adt_def == adt_def {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn associated_ty_value(
        &self,
        associated_ty_id: chalk_solve::rust_ir::AssociatedTyValueId<RustInterner<'tcx>>,
    ) -> Arc<chalk_solve::rust_ir::AssociatedTyValue<RustInterner<'tcx>>> {
        let def_id = associated_ty_id.0;
        let assoc_item = self.interner.tcx.associated_item(def_id);
        let impl_id = match assoc_item.container {
            AssocItemContainer::TraitContainer(def_id) => def_id,
            _ => unimplemented!("Not possible??"),
        };
        match assoc_item.kind {
            AssocKind::Type => {}
            _ => unimplemented!("Not possible??"),
        }
        let bound_vars = bound_vars_for_item(self.interner.tcx, def_id);
        let binders = binders_for(&self.interner, bound_vars);
        let ty = self.interner.tcx.type_of(def_id);

        Arc::new(chalk_solve::rust_ir::AssociatedTyValue {
            impl_id: chalk_ir::ImplId(impl_id),
            associated_ty_id: chalk_ir::AssocTypeId(def_id),
            value: chalk_ir::Binders::new(
                binders,
                chalk_solve::rust_ir::AssociatedTyValueBound { ty: ty.lower_into(&self.interner) },
            ),
        })
    }

    fn custom_clauses(&self) -> Vec<chalk_ir::ProgramClause<RustInterner<'tcx>>> {
        vec![]
    }

    fn local_impls_to_coherence_check(
        &self,
        _trait_id: chalk_ir::TraitId<RustInterner<'tcx>>,
    ) -> Vec<chalk_ir::ImplId<RustInterner<'tcx>>> {
        unimplemented!()
    }

    fn opaque_ty_data(
        &self,
        opaque_ty_id: chalk_ir::OpaqueTyId<RustInterner<'tcx>>,
    ) -> Arc<chalk_solve::rust_ir::OpaqueTyDatum<RustInterner<'tcx>>> {
        let bound_vars = bound_vars_for_item(self.interner.tcx, opaque_ty_id.0);
        let binders = binders_for(&self.interner, bound_vars);
        let where_clauses = self.where_clauses_for(opaque_ty_id.0, bound_vars);

        let value = chalk_solve::rust_ir::OpaqueTyDatumBound {
            bounds: chalk_ir::Binders::new(binders.clone(), vec![]),
            where_clauses: chalk_ir::Binders::new(binders, where_clauses),
        };
        Arc::new(chalk_solve::rust_ir::OpaqueTyDatum {
            opaque_ty_id,
            bound: chalk_ir::Binders::empty(&self.interner, value),
        })
    }

    fn program_clauses_for_env(
        &self,
        environment: &chalk_ir::Environment<RustInterner<'tcx>>,
    ) -> chalk_ir::ProgramClauses<RustInterner<'tcx>> {
        chalk_solve::program_clauses_for_env(self, environment)
    }

    fn well_known_trait_id(
        &self,
        well_known_trait: chalk_solve::rust_ir::WellKnownTrait,
    ) -> Option<chalk_ir::TraitId<RustInterner<'tcx>>> {
        use chalk_solve::rust_ir::WellKnownTrait::*;
        let def_id = match well_known_trait {
            Sized => self.interner.tcx.lang_items().sized_trait(),
            Copy => self.interner.tcx.lang_items().copy_trait(),
            Clone => self.interner.tcx.lang_items().clone_trait(),
            Drop => self.interner.tcx.lang_items().drop_trait(),
            Fn => self.interner.tcx.lang_items().fn_trait(),
            FnMut => self.interner.tcx.lang_items().fn_mut_trait(),
            FnOnce => self.interner.tcx.lang_items().fn_once_trait(),
            Unsize => self.interner.tcx.lang_items().unsize_trait(),
        };
        def_id.map(chalk_ir::TraitId)
    }

    fn is_object_safe(&self, trait_id: chalk_ir::TraitId<RustInterner<'tcx>>) -> bool {
        self.interner.tcx.is_object_safe(trait_id.0)
    }

    fn hidden_opaque_type(
        &self,
        _id: chalk_ir::OpaqueTyId<RustInterner<'tcx>>,
    ) -> chalk_ir::Ty<RustInterner<'tcx>> {
        // FIXME(chalk): actually get hidden ty
        self.interner
            .tcx
            .mk_ty(ty::Tuple(self.interner.tcx.intern_substs(&[])))
            .lower_into(&self.interner)
    }

    fn closure_kind(
        &self,
        _closure_id: chalk_ir::ClosureId<RustInterner<'tcx>>,
        substs: &chalk_ir::Substitution<RustInterner<'tcx>>,
    ) -> chalk_solve::rust_ir::ClosureKind {
        let kind = &substs.as_slice(&self.interner)[substs.len(&self.interner) - 3];
        match kind.assert_ty_ref(&self.interner).data(&self.interner) {
            chalk_ir::TyData::Apply(apply) => match apply.name {
                chalk_ir::TypeName::Scalar(scalar) => match scalar {
                    chalk_ir::Scalar::Int(int_ty) => match int_ty {
                        chalk_ir::IntTy::I8 => chalk_solve::rust_ir::ClosureKind::Fn,
                        chalk_ir::IntTy::I16 => chalk_solve::rust_ir::ClosureKind::FnMut,
                        chalk_ir::IntTy::I32 => chalk_solve::rust_ir::ClosureKind::FnOnce,
                        _ => bug!("bad closure kind"),
                    },
                    _ => bug!("bad closure kind"),
                },
                _ => bug!("bad closure kind"),
            },
            _ => bug!("bad closure kind"),
        }
    }

    fn closure_inputs_and_output(
        &self,
        _closure_id: chalk_ir::ClosureId<RustInterner<'tcx>>,
        substs: &chalk_ir::Substitution<RustInterner<'tcx>>,
    ) -> chalk_ir::Binders<chalk_solve::rust_ir::FnDefInputsAndOutputDatum<RustInterner<'tcx>>>
    {
        let sig = &substs.as_slice(&self.interner)[substs.len(&self.interner) - 2];
        match sig.assert_ty_ref(&self.interner).data(&self.interner) {
            chalk_ir::TyData::Function(f) => {
                let substitution = f.substitution.as_slice(&self.interner);
                let return_type =
                    substitution.last().unwrap().assert_ty_ref(&self.interner).clone();
                // Closure arguments are tupled
                let argument_tuple = substitution[0].assert_ty_ref(&self.interner);
                let argument_types = match argument_tuple.data(&self.interner) {
                    chalk_ir::TyData::Apply(apply) => match apply.name {
                        chalk_ir::TypeName::Tuple(_) => apply
                            .substitution
                            .iter(&self.interner)
                            .map(|arg| arg.assert_ty_ref(&self.interner))
                            .cloned()
                            .collect(),
                        _ => bug!("Expecting closure FnSig args to be tupled."),
                    },
                    _ => bug!("Expecting closure FnSig args to be tupled."),
                };

                chalk_ir::Binders::new(
                    chalk_ir::VariableKinds::from_iter(
                        &self.interner,
                        (0..f.num_binders).map(|_| chalk_ir::VariableKind::Lifetime),
                    ),
                    chalk_solve::rust_ir::FnDefInputsAndOutputDatum { argument_types, return_type },
                )
            }
            _ => panic!("Invalid sig."),
        }
    }

    fn closure_upvars(
        &self,
        _closure_id: chalk_ir::ClosureId<RustInterner<'tcx>>,
        substs: &chalk_ir::Substitution<RustInterner<'tcx>>,
    ) -> chalk_ir::Binders<chalk_ir::Ty<RustInterner<'tcx>>> {
        let inputs_and_output = self.closure_inputs_and_output(_closure_id, substs);
        let tuple = substs.as_slice(&self.interner).last().unwrap().assert_ty_ref(&self.interner);
        inputs_and_output.map_ref(|_| tuple.clone())
    }

    fn closure_fn_substitution(
        &self,
        _closure_id: chalk_ir::ClosureId<RustInterner<'tcx>>,
        substs: &chalk_ir::Substitution<RustInterner<'tcx>>,
    ) -> chalk_ir::Substitution<RustInterner<'tcx>> {
        let substitution = &substs.as_slice(&self.interner)[0..substs.len(&self.interner) - 3];
        chalk_ir::Substitution::from_iter(&self.interner, substitution)
    }
}

/// Creates a `InternalSubsts` that maps each generic parameter to a higher-ranked
/// var bound at index `0`. For types, we use a `BoundVar` index equal to
/// the type parameter index. For regions, we use the `BoundRegion::BrNamed`
/// variant (which has a `DefId`).
fn bound_vars_for_item(tcx: TyCtxt<'tcx>, def_id: DefId) -> SubstsRef<'tcx> {
    InternalSubsts::for_item(tcx, def_id, |param, substs| match param.kind {
        ty::GenericParamDefKind::Type { .. } => tcx
            .mk_ty(ty::Bound(
                ty::INNERMOST,
                ty::BoundTy {
                    var: ty::BoundVar::from(param.index),
                    kind: ty::BoundTyKind::Param(param.name),
                },
            ))
            .into(),

        ty::GenericParamDefKind::Lifetime => tcx
            .mk_region(ty::RegionKind::ReLateBound(
                ty::INNERMOST,
                ty::BoundRegion::BrAnon(substs.len() as u32),
            ))
            .into(),

        ty::GenericParamDefKind::Const => tcx
            .mk_const(ty::Const {
                val: ty::ConstKind::Bound(ty::INNERMOST, ty::BoundVar::from(param.index)),
                ty: tcx.type_of(param.def_id),
            })
            .into(),
    })
}

fn binders_for<'tcx>(
    interner: &RustInterner<'tcx>,
    bound_vars: SubstsRef<'tcx>,
) -> chalk_ir::VariableKinds<RustInterner<'tcx>> {
    chalk_ir::VariableKinds::from_iter(
        interner,
        bound_vars.iter().map(|arg| match arg.unpack() {
            ty::subst::GenericArgKind::Lifetime(_re) => chalk_ir::VariableKind::Lifetime,
            ty::subst::GenericArgKind::Type(_ty) => {
                chalk_ir::VariableKind::Ty(chalk_ir::TyKind::General)
            }
            ty::subst::GenericArgKind::Const(c) => {
                chalk_ir::VariableKind::Const(c.ty.lower_into(interner))
            }
        }),
    )
}
