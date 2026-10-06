/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Reserved for an eventual `Type::External` dispatch path. uniffi removed
// `Type::External` from the public `Type` enum, so external types now arrive
// as `Type::{Object,Record,Enum,...}` with a non-local `module_path`. We
// currently surface those via `using uniffi.<crate>;` directives emitted in
// `CsWrapper::new`; keep this stub matching upstream until the dispatch
// path is reintroduced.
#![allow(dead_code)]

use super::CodeType;
use uniffi_bindgen::{interface::Literal, ComponentInterface};

#[derive(Debug)]
pub struct ExternalCodeType {
    name: String,
}

impl CodeType for ExternalCodeType {
    fn type_label(&self, _ci: &ComponentInterface) -> String {
        self.name.clone()
    }

    fn canonical_name(&self) -> String {
        format!("Type{}", self.name)
    }

    fn literal(&self, _literal: &Literal, _ci: &ComponentInterface) -> String {
        unreachable!("Can't have a literal of an external type");
    }
}
