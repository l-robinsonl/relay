/**
 * Support for the `*.shot.test.ts` cameras. See `scripts/shoot-ui.sh`.
 *
 * Not used by any real code — only by the fixtures that photograph
 * components.
 */
import type { VueWrapper } from '@vue/test-utils'

/**
 * The markup a component actually put on the page.
 *
 * `wrapper.html()` is not enough: every dialog in this app wraps its
 * body in `<Teleport to="body">`, and teleported content is not part
 * of the wrapper's own tree. Photographing AssistantSettings that way
 * produced an empty box, which looks exactly like a broken component
 * and is not one.
 */
export function rendered (wrapper: VueWrapper<never>): string {
  const teleported = Array.from(document.body.children)
    .filter((el) => !el.contains(wrapper.element))
    .map((el) => el.outerHTML)
    .join('\n')

  return teleported || wrapper.html()
}
