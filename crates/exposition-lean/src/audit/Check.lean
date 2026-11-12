/-
Exposition's Lean audit, step 2 of 2: what the checked text's declarations
really rest on, decided from data by a process that runs none of the checked
file's code and none of its imports' code.

* This program imports only the toolchain's own modules: the application puts
  the toolchain's library first on `LEAN_PATH`, so a project cannot shadow
  `Init`/`Std`/`Lean`.
* The checked module and every non-toolchain module it imports are read as
  `.olean` data. No environment extension is initialized from them, and no
  `initialize` block, attribute handler, elaborator, macro, or option of theirs
  runs. The toolchain modules are imported the same way (`loadExts := false`).
* Kernel replay: every non-toolchain declaration reachable from the checked
  module's declarations is checked again by Lean's kernel on top of the
  toolchain's own environment (`Lean.Kernel.Environment.replay`, as
  lean4checker does). A declaration that was added without the kernel
  (`debug.skipKernelTC` however it was set, `addDeclWithoutChecking`, or any
  other elaborator trick) is rejected here.
* Axioms are collected from the replayed declarations by walking their terms.
  An axiom is reported with whether it is the toolchain's own constant, so a
  project declaration named like a standard axiom never counts as standard.

Output: lines `<tag> <json>` on stdout; the last one is `{"end":true}`.
-/
import Lean.Replay
import Lean.Util.CollectAxioms
import Lean.Util.Path

open Lean

namespace ExpositionAuditTool.Check

def emit (tag : String) (j : Json) : IO Unit := do
  let out ← IO.getStdout
  out.putStrLn s!"{tag} {j.compress}"
  out.flush

/-- A module's `.olean` data: every declaration (the private part of a module-system file). -/
def readModule (file : System.FilePath) : IO ModuleData := do
  let parts ← readModuleDataParts #[file]
  let some (main, _) := parts[0]? | throw <| IO.userError s!"could not read {file}"
  if !main.isModule then
    return main
  let parts ← readModuleDataParts
    #[file, OLeanLevel.server.adjustFileName file, OLeanLevel.private.adjustFileName file]
  let some (all, _) := parts.back? | throw <| IO.userError s!"could not read {file}"
  return all

instance : MonadEnv (StateM Environment) where
  getEnv := get
  modifyEnv := modify

def axiomsOf (env : Environment) (n : Name) : Array Name :=
  (collectAxioms n : StateM Environment (Array Name)).run' env

/-- `[tag, toolchain-lib, module]`. -/
def run (args : List String) : IO UInt32 := do
  let [tag, toolchainLib, moduleName] := args
    | throw <| IO.userError "usage: <tag> <toolchain-lib> <module>"
  let module := moduleName.toName
  let libPrefix := toolchainLib ++ System.FilePath.pathSeparator.toString
  let fail (message : String) : IO UInt32 := do
    emit tag (Json.mkObj [("error", toJson message)])
    emit tag (Json.mkObj [("end", true)])
    return 1
  -- The checked module and the non-toolchain modules it imports, as data.
  let mut queue := #[module]
  let mut seen : NameSet := {}
  let mut toolchainImports : Array Import := #[]
  let mut projectModules : Array (Name × ModuleData) := #[]
  while h : queue.size > 0 do
    let m := queue.back
    queue := queue.pop
    if seen.contains m then continue
    seen := seen.insert m
    let file ← findOLean m
    if file.toString.startsWith libPrefix then
      if m == module then return ← fail "the checked module resolved into the toolchain"
      toolchainImports := toolchainImports.push { module := m }
      continue
    let data ← readModule file
    projectModules := projectModules.push (m, data)
    for i in data.imports do
      queue := queue.push i.module
  let some (_, checked) := projectModules.find? (·.1 == module)
    | return ← fail "the checked module was not found"
  let mut project : Std.HashMap Name ConstantInfo := {}
  for (_, data) in projectModules do
    for c in data.constants do
      project := project.insert c.name c
  -- Non-toolchain declarations reachable from the checked module's declarations.
  let mut todo : Array Name := checked.constants.map (·.name)
  let mut closure : Std.HashMap Name ConstantInfo := {}
  while h : todo.size > 0 do
    let n := todo.back
    todo := todo.pop
    if closure.contains n then continue
    let some ci := project[n]? | continue
    closure := closure.insert n ci
    todo := todo ++ ci.type.getUsedConstants
    if let some v := ci.value? (allowOpaque := true) then
      todo := todo ++ v.getUsedConstants
    match ci with
    | .inductInfo v => todo := todo ++ v.all.toArray ++ v.ctors.toArray
    | .ctorInfo v => todo := todo.push v.induct
    | .recInfo v => todo := todo ++ v.all.toArray
    | _ => pure ()
  -- The toolchain alone, as data.
  let base ← importModules toolchainImports {} (loadExts := false)
  -- Kernel replay of the closure on top of the toolchain.
  let kernel ← try
      pure (Except.ok (← base.toKernelEnv.replay closure))
    catch e => pure (Except.error (toString e))
  match kernel with
  | .error message =>
    emit tag (Json.mkObj [("kernel", toJson (message.take 4000).copy), ("replayed", toJson closure.size)])
    emit tag (Json.mkObj [("end", true)])
    return 0
  | .ok kenv =>
    emit tag (Json.mkObj [("kernel", Json.null), ("replayed", toJson closure.size)])
    let env := Environment.ofKernelEnv kenv
    for ci in checked.constants do
      let n := ci.name
      -- Unsafe and partial declarations are not sent to the kernel (as in lean4checker);
      -- safe declarations cannot use them in proofs.
      let replayed := !ci.isUnsafe && !ci.isPartial
      let axs := if replayed then axiomsOf env n else #[]
      let ax := axs.map fun a => Json.mkObj [("n", toJson a.toString), ("t", toJson (base.find? a).isSome)]
      emit tag (Json.mkObj [("n", toJson n.toString), ("i", toJson n.isInternal), ("k", toJson replayed), ("ax", Json.arr ax)])
    emit tag (Json.mkObj [("end", true)])
    return 0

end ExpositionAuditTool.Check

def main (args : List String) : IO UInt32 := ExpositionAuditTool.Check.run args
