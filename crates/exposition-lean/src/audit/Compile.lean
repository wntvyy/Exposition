/-
Exposition's Lean audit, step 1 of 2: elaborate exactly the checked text as a
temporary module and write its `.olean`.

This process runs the checked file's own code (that is what elaborating it
means), so nothing it prints is trusted except the single result line tagged
with the per-run secret. The secret is read from stdin, which the application
closes right after writing it, before any of the file's code (or the code of
its imports) can run. The result says whether elaboration reached the end of
the text: Lean's frontend stops at a terminal command (`#exit`, a misplaced
`import`), and this is read from the frontend's own command snapshots, not
from anything the file can declare.

Step 2 (`Check.lean`) never runs any of the file's code.
-/
import Lean.Elab.Frontend

open Lean

namespace ExpositionAuditTool.Compile

/-- The last command snapshot: the terminal command that ended elaboration. -/
partial def lastCommand (s : Language.Lean.CommandParsedSnapshot) : Language.Lean.CommandParsedSnapshot :=
  match s.nextCmdSnap? with
  | some next => lastCommand next.get
  | none => s

def report (secret : String) (fields : List (String × Json)) : IO Unit := do
  let out ← IO.getStdout
  -- On a line of its own even if the file left a partial line on stdout.
  out.putStr s!"\n{secret} {(Json.mkObj fields).compress}\n"
  out.flush

/-- `[source, olean, module]`; stdin: the secret. Exit code 0 when the
`.olean` was written. -/
def run (args : List String) : IO UInt32 := do
  -- Before anything else, and before the checked file's code exists in this process.
  let secret := (← (← IO.getStdin).getLine).trimAscii.copy
  unless secret.length ≥ 32 && secret.all Char.isAlphanum do
    throw <| IO.userError "the audit secret is missing"
  let [source, olean, module] := args
    | throw <| IO.userError "usage: <source> <olean> <module>"
  let some input := String.fromUTF8? (← IO.FS.readBinFile source)
    | report secret [("error", "the checked text is not valid UTF-8")]; return 1
  -- Same frontend setup as the `lean` command line (which enables initializers).
  unsafe enableInitializersExecution
  let opts := Lean.internal.cmdlineSnapshots.setIfNotSet {} true
  let opts := Elab.async.setIfNotSet opts true
  let ictx := Parser.mkInputContext input source
  let mainModuleName := module.toName
  let setup (stx : Elab.HeaderSyntax) :
      Language.ProcessingT IO (Except Language.Lean.HeaderProcessedSnapshot Language.Lean.SetupImportsResult) :=
    return .ok { imports := stx.imports, isModule := stx.isModule, mainModuleName, opts, trustLevel := 0 }
  let snap ← Language.Lean.process setup none { ictx with }
  let hasErrors ← (Language.toSnapshotTree snap).runAndReport opts (json := true)
  let some processed := snap.result?.bind (·.processedSnap.get.result?)
    | report secret [("error", "the imports of the checked text could not be loaded")]; return 1
  let last := lastCommand processed.firstCmdSnap.get
  let complete := last.stx.isOfKind ``Parser.Command.eoi
  let stop := ictx.fileMap.utf8PosToLspPos (last.stx.getPos?.getD last.parserState.pos)
  if hasErrors then
    report secret [("error", "Lean reported errors when compiling the checked text")]; return 1
  let some cmdState := Language.Lean.waitForFinalCmdState? snap
    | report secret [("error", "Lean produced no final state")]; return 1
  let env := cmdState.env
  writeModule env olean
  -- Declaration ranges (only used to point at declarations in the editor).
  let ranges : Array Json := env.constants.foldStage2 (fun acc n _ =>
    match declRangeExt.find? env n with
    | some r => acc.push (Json.arr #[toJson n.toString, toJson r.selectionRange.pos.line,
        toJson r.selectionRange.charUtf16, toJson r.selectionRange.endPos.line, toJson r.selectionRange.endCharUtf16])
    | none => acc) #[]
  report secret [("complete", toJson complete), ("stop", toJson [stop.line, stop.character]), ("ranges", Json.arr ranges)]
  return 0

end ExpositionAuditTool.Compile

def main (args : List String) : IO UInt32 := ExpositionAuditTool.Compile.run args
