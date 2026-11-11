/**
 * Vite serves any module with a `?raw` suffix as its own source text. Used by
 * the invariant suites, which assert about what a file says rather than what
 * it does, and this keeps that possible without pulling `@types/node` into a
 * package that is otherwise pure, dependency-free TypeScript.
 */
declare module '*?raw' {
  const content: string
  export default content
}
