//! TypeScript bindings for every wire type, generated from the Rust definitions.
//!
//! The output is one self-contained file, checked in at `apps/web/src/contracts/index.ts`.
//! Regenerate it with:
//!
//! ```sh
//! PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript
//! ```
//!
//! Without the variable, that test fails when the checked-in file is stale, so CI catches a
//! forgotten regeneration.

use std::any::TypeId;
use std::collections::{BTreeMap, HashSet};

use ts_rs::{TS, TypeVisitor};

use crate::catalog::{
    CalendarEventData, EventTypeInfo, FsBurstData, FsFileData, FsMovedData, GitBranchSwitchedData,
    GitCommitData, GitMergeConflictedData, GitPushCompletedData, ProcessExitedData,
    ScheduleFiredData,
};
use crate::rpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, Notification};
use crate::status::{DaemonInfo, UiLoginRequest, UiLoginResponse};

struct Collect<'a> {
    cfg: &'a ts_rs::Config,
    seen: HashSet<TypeId>,
    decls: BTreeMap<String, String>,
}

impl TypeVisitor for Collect<'_> {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        if T::output_path().is_none() || !self.seen.insert(TypeId::of::<T>()) {
            return;
        }
        self.decls.insert(T::ident(self.cfg), T::decl(self.cfg));
        T::visit_dependencies(self);
    }
}

/// The full contents of `apps/web/src/contracts/index.ts`.
pub fn typescript_bindings() -> String {
    let cfg = ts_rs::Config::new().with_large_int("number");
    let mut c = Collect { cfg: &cfg, seen: HashSet::new(), decls: BTreeMap::new() };
    crate::rpc::visit_method_types(&mut c);
    c.visit::<Notification>();
    c.visit::<JsonRpcRequest>();
    c.visit::<JsonRpcResponse>();
    c.visit::<JsonRpcNotification>();
    c.visit::<DaemonInfo>();
    c.visit::<UiLoginRequest>();
    c.visit::<UiLoginResponse>();
    c.visit::<EventTypeInfo>();
    c.visit::<FsFileData>();
    c.visit::<FsMovedData>();
    c.visit::<FsBurstData>();
    c.visit::<GitCommitData>();
    c.visit::<GitBranchSwitchedData>();
    c.visit::<GitMergeConflictedData>();
    c.visit::<GitPushCompletedData>();
    c.visit::<ProcessExitedData>();
    c.visit::<ScheduleFiredData>();
    c.visit::<CalendarEventData>();

    let mut out = String::from(
        "// Generated from crates/pulse-contracts. Do not edit.\n\
         // Regenerate: PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript\n\n",
    );
    for decl in c.decls.values() {
        out.push_str("export ");
        out.push_str(decl.trim());
        out.push_str("\n\n");
    }
    out.push_str(
        "/** Every RPC method with its params and result types. */\nexport type RpcMethods = {\n",
    );
    let table = crate::rpc::ts_method_table(&cfg);
    for (name, params, result) in &table {
        out.push_str(&format!("  \"{name}\": {{ params: {params}; result: {result} }};\n"));
    }
    out.push_str("};\n\nexport type RpcMethodName = keyof RpcMethods;\n\n");
    out.push_str("export const RPC_METHOD_NAMES = [\n");
    for (name, _, _) in &table {
        out.push_str(&format!("  \"{name}\",\n"));
    }
    out.push_str("] as const satisfies readonly RpcMethodName[];\n\n");
    out.push_str("export type NotificationMethod = Notification[\"method\"];\n");
    out.push_str(
        "export type NotificationParams<M extends NotificationMethod> = Extract<Notification, { method: M }>[\"params\"];\n",
    );
    out
}
