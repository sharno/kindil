------------------------------- MODULE Launcher -------------------------------
(***************************************************************************)
(* kindil's session (launcher/src/session.rs) and its update trial         *)
(* (src/update.rs `Slots`): Amazon's services step aside while kindil runs *)
(* and come back on every way out; a staged update gets one trial run and  *)
(* is kept only once kindil has run SETTLE and committed it; crashes       *)
(* restart kindil, CRASHES within WINDOW move to the previous version, and *)
(* CRASHES more give up.                                                   *)
(*                                                                         *)
(* Files, by the version they hold (0: absent): in the extension's bin/,   *)
(* `kindil` (installed), `kindil.previous`, `kindil.next` (staged) and     *)
(* `kindil.trying` (the trial); in /var/local/kindil, the marker `trying`  *)
(* and the copy that runs, `kindil` (with `kindil.from`). A reboot (or     *)
(* SIGKILL, or a power cut) can come at any step; it restarts every        *)
(* service and keeps the files.                                            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxVersion,  \* versions 1..MaxVersion; each staged update is a newer one
    CRASHES,     \* session.rs CRASHES
    CopyFails,   \* copying to the system storage can fail (no room)
    CommitCut    \* kindil can exit, or the Kindle reboot, between the commit's renames

(* What the session stops (the stopper thread stops Amazon's SERVICES and, *)
(* with kindil's USB drive mode, volumd and USBNetwork's jobs).            *)
Services == {"lab126_gui", "x", "powerd", "SERVICES", "volumd"}
None == 0
Versions == 1..MaxVersion

VARIABLES
    phase,       \* "off" (no session), "running" (kindil runs), "done" (restored)
    up,          \* Amazon's services running
    usb,         \* /var/local/kindil/usb-drive holds kindil: volumd is stopped too
    installed, previous, next, trial,  \* bin/kindil, kindil.previous, kindil.next, kindil.trying
    trying,      \* the marker: a trial is under way
    copy,        \* the version /var/local/kindil/kindil holds
    runs,        \* the version kindil runs now (None: not running)
    slot,        \* "installed" or "previous": what the session runs when no trial
    crashes,     \* crashes within WINDOW
    commit,      \* where kindil's `Slots::commit` is: 0 not started, 1, 2 its renames done
    \* Ghosts
    failed,      \* versions whose trial ended before kindil set out to commit them
    before,      \* the installed version when the trial under way started
    wrongNext    \* a run after a trial ended early wasn't the version before it

vars == <<phase, up, usb, installed, previous, next, trial, trying, copy, runs, slot,
          crashes, commit, failed, before, wrongNext>>

Init ==
    /\ phase = "off"
    /\ up = Services
    /\ usb \in BOOLEAN
    /\ installed \in Versions /\ previous \in {None} \cup Versions
    /\ previous < installed
    /\ next = None /\ trial = None /\ trying = FALSE
    /\ copy = None /\ runs = None
    /\ slot = "installed" /\ crashes = 0 /\ commit = 0
    /\ failed = {} /\ before = None /\ wrongNext = FALSE

SlotVersion(s, inst, prev) == IF s = "installed" THEN inst ELSE prev

(* `Files::pick` then `os.run`: a staged update is moved to the trial,    *)
(* copied, and the marker written: it runs on trial. Otherwise (or if it   *)
(* can't be copied: dropped) the slot's binary is copied, if the copy      *)
(* isn't current, and runs; one that can't be copied runs where it is; no  *)
(* slot binary at all runs the copy left from before. `inst`, `prev`,      *)
(* `nxt`, `trl`, `tr`: the files and the marker as the pick finds them.    *)
Pick(s, inst, prev, nxt, trl, tr) ==
    \E ok \in (IF CopyFails THEN BOOLEAN ELSE {TRUE}) :
    LET v == SlotVersion(s, inst, prev) IN
    /\ next' = None
    /\ IF nxt # None /\ ok
       THEN /\ trial' = nxt /\ trying' = TRUE /\ copy' = nxt /\ runs' = nxt
            /\ before' = v
       ELSE /\ trial' = (IF nxt # None THEN None ELSE trl)
            /\ trying' = tr
            /\ copy' = IF v # None /\ ok THEN v ELSE copy
            /\ runs' = IF v # None THEN v ELSE copy
            /\ UNCHANGED before

(* `launcher start` from KUAL or the boot job: the session. A trial cut   *)
(* short by a reboot is dropped first.                                     *)
Start ==
    /\ phase = "off"
    /\ failed' = IF trying /\ trial # None /\ installed # None THEN failed \cup {trial} ELSE failed
    /\ Pick("installed", installed, previous, next, IF trying THEN None ELSE trial, FALSE)
    /\ phase' = "running"
    /\ up' = up \ {"lab126_gui", "x", "powerd"}
    /\ slot' = "installed" /\ crashes' = 0 /\ commit' = 0
    /\ UNCHANGED <<usb, installed, previous, wrongNext>>

(* The stopper thread's rounds, until kindil ends. *)
Stopper ==
    /\ phase = "running"
    /\ \E s \in {"SERVICES"} \cup (IF usb THEN {"volumd"} ELSE {}) :
         /\ s \in up
         /\ up' = up \ {s}
    /\ UNCHANGED <<phase, usb, installed, previous, next, trial, trying, copy, runs, slot,
                   crashes, commit, failed, before, wrongNext>>

(* kindil stages an update (`Slots::stage`): the next version. *)
Stage ==
    /\ phase = "running"
    /\ \E v \in Versions :
         /\ v > installed /\ v > next /\ v > trial
         \* Each trial is of a build of its own.
         /\ v \notin failed
         /\ next' = v
    /\ UNCHANGED <<phase, up, usb, installed, previous, trial, trying, copy, runs, slot,
                   crashes, commit, failed, before, wrongNext>>

(* kindil, on trial for SETTLE, commits (`Slots::commit`): kindil →        *)
(* kindil.previous, kindil.trying → kindil, the marker removed; each a     *)
(* step of its own.                                                        *)
Commit ==
    /\ phase = "running" /\ trying
    /\ \/ /\ commit = 0 /\ trial # None /\ commit' = 1
          /\ previous' = IF installed # None THEN installed ELSE previous
          /\ installed' = None
          /\ UNCHANGED <<trial, trying>>
       \/ /\ commit = 1 /\ commit' = 2
          /\ installed' = trial /\ trial' = None
          /\ UNCHANGED <<previous, trying>>
       \/ /\ commit = 2 /\ commit' = 0 /\ trying' = FALSE
          /\ UNCHANGED <<installed, previous, trial>>
       \* Replaced by hand mid-trial: nothing to keep.
       \/ /\ commit = 0 /\ trial = None /\ trying' = FALSE
          /\ UNCHANGED <<installed, previous, trial, commit>>
    /\ UNCHANGED <<phase, up, usb, next, copy, runs, slot, crashes, failed, before, wrongNext>>

(* `restore`: everything Amazon's UI needs, started if not running. *)
Restore == up' = Services

(* kindil exits, and `run` decides (session.rs). *)
Exit ==
    /\ phase = "running"
    /\ CommitCut \/ commit = 0
    /\ \E status \in {"ok", "restart", "killed", "crash"} :
       IF trying
       THEN \* The trial ended before kindil kept it: back to the previous binary.
            /\ failed' = IF commit = 0 THEN failed \cup {runs} ELSE failed
            /\ IF status = "ok"
               THEN /\ phase' = "done" /\ Restore /\ runs' = None
                    /\ trial' = None /\ trying' = FALSE
                    /\ UNCHANGED <<next, copy, before, wrongNext>>
               ELSE /\ UNCHANGED <<phase, up>>
                    /\ Pick(slot, installed, previous, next, None, FALSE)
                    /\ wrongNext' = (wrongNext \/ (commit = 0 /\ next = None /\ runs' # before))
            /\ UNCHANGED <<slot, crashes>>
       ELSE /\ UNCHANGED <<failed, wrongNext>>
            /\ CASE status = "restart" ->
                      /\ Pick(slot, installed, previous, next, trial, trying)
                      /\ UNCHANGED <<phase, up, slot, crashes>>
                 [] status \in {"ok", "killed"} ->
                      /\ phase' = "done" /\ Restore /\ runs' = None
                      /\ UNCHANGED <<next, trial, trying, copy, before, slot, crashes>>
                 [] OTHER ->
                      IF crashes + 1 < CRASHES
                      THEN /\ crashes' = crashes + 1
                           /\ Pick(slot, installed, previous, next, trial, trying)
                           /\ UNCHANGED <<phase, up, slot>>
                      ELSE IF slot = "installed" /\ previous # None
                      THEN /\ slot' = "previous" /\ crashes' = 0
                           /\ Pick("previous", installed, previous, next, trial, trying)
                           /\ UNCHANGED <<phase, up>>
                      ELSE /\ phase' = "done" /\ Restore /\ runs' = None
                           /\ UNCHANGED <<next, trial, trying, copy, before, slot, crashes>>
    /\ commit' = 0
    /\ UNCHANGED <<usb, installed, previous>>

(* WINDOW passes without a crash. *)
Forget ==
    /\ phase = "running" /\ crashes > 0
    /\ crashes' = 0
    /\ UNCHANGED <<phase, up, usb, installed, previous, next, trial, trying, copy, runs, slot,
                   commit, failed, before, wrongNext>>

(* A reboot, SIGKILL or a power cut, at any step: the services come back  *)
(* with the boot; the files stay.                                          *)
Reboot ==
    /\ phase # "off"
    /\ CommitCut \/ commit = 0
    /\ phase' = "off" /\ up' = Services /\ runs' = None /\ commit' = 0
    /\ UNCHANGED <<usb, installed, previous, next, trial, trying, copy, slot, crashes,
                   failed, before, wrongNext>>

Next == Start \/ Stopper \/ Stage \/ Commit \/ Exit \/ Forget \/ Reboot

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
TypeOK ==
    /\ phase \in {"off", "running", "done"}
    /\ up \subseteq Services
    /\ {installed, previous, next, trial, copy, runs} \subseteq {None} \cup Versions
    /\ slot \in {"installed", "previous"}
    /\ crashes \in 0..CRASHES

(* Every way out of a session restores what it stopped. *)
RestoredWhenDone == phase = "done" => up = Services

(* A version whose trial ended before it was kept is never kept. *)
FailedTrialNeverKept == \A v \in failed : installed # v

(* After a trial ends early, the version installed before it runs next. *)
PreviousRunsNext == ~wrongNext

(* The extension keeps an installed binary, but while a commit is under    *)
(* way: KUAL's start and the boot job run it.                              *)
InstalledKept == commit = 0 => installed # None
=============================================================================
