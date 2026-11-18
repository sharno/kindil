# State machines

kindil's step-by-step procedures are [statum](https://docs.rs/statum)
typestate machines: each step is a method of the state before it only, so
a step out of order doesn't compile. These graphs are what the macros
generate (statum's `introspection`), sorted; `cargo xtask state-machines`
writes them, and `cargo xtask check` fails while they are out of date.
Each line reads `State.method -> legal targets`: a method returning a
`Result` lists both of its machine targets. States a machine is rebuilt
into from files or from the device (`#[validators]`) are where it starts
again in a later run, not edges: the update trial's Staged and Trying, the
storage's Mounted, Released and Exported.

<!-- kindil: begin -->
## kindil install

src/install.rs: a release installs itself, and rolls back unless it comes up and holds.

`install::steps::Installing`: Checked, BackedUp, Stopped, Placed, Started, Up, Kept, Failed, RolledBack.

```mermaid
stateDiagram-v2
    %% machine: install::steps::Installing
    state "Checked" as s0
    state "BackedUp" as s1
    state "Stopped" as s2
    state "Placed" as s3
    state "Started" as s4
    state "Up" as s5
    state "Kept" as s6
    state "Failed" as s7
    state "RolledBack" as s8
    s1 --> s2: stop
    s0 --> s1: back_up
    s7 --> s8: roll_back
    s3 --> s4: start
    s4 --> s5: come_up
    s4 --> s7: come_up
    s2 --> s3: place
    s2 --> s7: place
    s5 --> s6: hold
    s5 --> s7: hold
```

```text
BackedUp.stop -> Stopped
Checked.back_up -> BackedUp
Failed.roll_back -> RolledBack
Placed.start -> Started
Started.come_up -> Up | Failed
Stopped.place -> Placed | Failed
Up.hold -> Kept | Failed
```

## Over-the-air update

src/update.rs: kindil's side of an update; the launcher runs the trial (below).

`update::steps::Ota`: Idle, Available, Verified, Staged, Trying, Kept, RolledBack.

```mermaid
stateDiagram-v2
    %% machine: update::steps::Ota
    state "Idle" as s0
    state "Available" as s1
    state "Verified" as s2
    state "Staged" as s3
    state "Trying" as s4
    state "Kept" as s5
    state "RolledBack" as s6
    s1 --> s2: download
    s1 --> s2: verify
    s3 --> s6: roll_back
    s4 --> s5: commit
    s4 --> s6: roll_back
    s2 --> s3: stage
```

```text
Available.download -> Verified
Available.verify -> Verified
Staged.roll_back -> RolledBack
Trying.commit -> Kept
Trying.roll_back -> RolledBack
Verified.stage -> Staged
```

## USB drive mode

src/drive.rs: the user storage to a computer and back.

`drive::sequence::Storage`: Mounted, Unmounted, Free, GadgetIn, Exported, Released, Checked, Remounted.

```mermaid
stateDiagram-v2
    %% machine: drive::sequence::Storage
    state "Mounted" as s0
    state "Unmounted" as s1
    state "Free" as s2
    state "GadgetIn" as s3
    state "Exported" as s4
    state "Released" as s5
    state "Checked" as s6
    state "Remounted" as s7
    s6 --> s7: mount
    s4 --> s5: release
    s2 --> s3: load_gadget
    s3 --> s4: export
    s0 --> s1: unmount
    s5 --> s6: check
    s1 --> s2: verify
```

```text
Checked.mount -> Remounted
Exported.release -> Released
Free.load_gadget -> GadgetIn
GadgetIn.export -> Exported
Mounted.unmount -> Unmounted
Released.check -> Checked
Unmounted.verify -> Free
```

## Suspend (naps and sleeps)

src/suspend.rs: every suspend; the caller has parked Wi-Fi.

`suspend::Suspend`: Awake, Armed, UntilButton, Resumed.

```mermaid
stateDiagram-v2
    %% machine: suspend::Suspend
    state "Awake" as s0
    state "Armed" as s1
    state "UntilButton" as s2
    state "Resumed" as s3
    s1 --> s3: suspend
    s0 --> s1: arm
    s0 --> s1: arm_or_button
    s0 --> s2: arm_or_button
    s0 --> s2: until_button
    s2 --> s3: suspend
```

```text
Armed.suspend -> Resumed
Awake.arm -> Armed
Awake.arm_or_button -> Armed | UntilButton
Awake.until_button -> UntilButton
UntilButton.suspend -> Resumed
```

<!-- kindil: end -->
<!-- launcher: begin -->
## Launcher session

launcher/src/session.rs: Amazon's UI steps aside while kindil runs, and always comes back.

`session::lifecycle::Session`: Starting, Ready, Aside, Restored.

```mermaid
stateDiagram-v2
    %% machine: session::lifecycle::Session
    state "Starting" as s0
    state "Ready" as s1
    state "Aside" as s2
    state "Restored" as s3
    s2 --> s3: restore
    s1 --> s2: step_aside
    s0 --> s1: pick
```

```text
Aside.restore -> Restored
Ready.step_aside -> Aside
Starting.pick -> Ready
```

## Update trial (launcher)

launcher/src/session.rs: a staged update's one trial run; kindil keeps it (above).

`session::steps::Trial`: Idle, Staged, Trying.

```mermaid
stateDiagram-v2
    %% machine: session::steps::Trial
    state "Idle" as s0
    state "Staged" as s1
    state "Trying" as s2
    s1 --> s2: begin
    s1 --> s0: begin
    s2 --> s0: roll_back
```

```text
Staged.begin -> Trying | Idle
Trying.roll_back -> Idle
```

<!-- launcher: end -->
