import { describe, expect, it } from 'vitest'

import { callStackHtml, type CallFrame } from '../src/panels/callstack-panel'
import { describeShape, recordedIn } from './reader-shapes'

// The call stack `getCallStack(0)` gives at the stop of the Mega Drive
// binding's own test (`what_the_call_stack_holds`): the JSR at $43C, which
// returns to $442, and no name.
const stack: CallFrame[] = [{ call_address: 0x43c, function_name: undefined, return_address: 0x442 }]

describe('the call stack panel', () => {
  it('reads the frames as the binding records them', () => {
    expect(recordedIn('cores/sg1/wasm/src/tests/debugger_shape_tests.rs')).toContain(
      describeShape(stack),
    )
  })

  it('shows where each call is, where it returns, and (unknown) with no name', () => {
    const html = callStackHtml(stack)
    expect(html).toContain('<span class="callstack-address">00043C</span>')
    expect(html).toContain('→ 000442</span>')
    expect(html).toContain('<span class="callstack-function">(unknown)</span>')
    expect(html).not.toContain('?')
  })

  it("shows a function's name, escaped", () => {
    const html = callStackHtml([{ ...stack[0], function_name: 'draw<sprites>' }])
    expect(html).toContain('<span class="callstack-function">draw&lt;sprites&gt;</span>')
  })

  it('says when the stack is empty', () => {
    expect(callStackHtml([])).toContain('Call stack empty')
  })
})
