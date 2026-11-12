# Slint virtual keyboard

Source: `slint-ui/slint`, tag `v1.18.1`,
`examples/virtual_keyboard/ui/{virtual_keyboard.slint,icons.slint,assets/*}`.
The source headers identify the example as MIT; the upstream MIT license text
is retained here. Copyright © SixtyFPS GmbH <info@slint.dev>.

Local changes: 60px-high monochrome keys, no repaint animations, complete
printable-ASCII symbol coverage, Arabic layout, and wrap-to-first layout cycling.
Kindil dispatches the example's keys as Slint window events; Slint owns editing,
selection, cursor movement, Unicode shaping and password masking.
