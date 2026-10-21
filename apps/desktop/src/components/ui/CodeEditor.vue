<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue'
import type { EditorView } from '@codemirror/view'

/**
 * A CodeMirror 6 editor.
 *
 * CodeMirror is loaded on demand rather than imported at the top level: it is
 * the single largest dependency in the bundle, and the application shell
 * should not wait for it before painting. Until it arrives the textarea
 * fallback is fully usable, so typing is never blocked.
 */
const props = withDefaults(
  defineProps<{
    modelValue: string
    language?: 'json' | 'xml' | 'javascript' | 'graphql' | 'text'
    readonly?: boolean
    placeholder?: string
  }>(),
  { language: 'text', readonly: false, placeholder: '' },
)

const emit = defineEmits<{ 'update:modelValue': [string] }>()

const host = ref<HTMLDivElement | null>(null)
const view = shallowRef<EditorView | null>(null)
const ready = ref(false)

async function mount () {
  if (!host.value) return

  const [
    { EditorView: View, keymap, lineNumbers, highlightActiveLine, placeholder: placeholderExt },
    { EditorState, Compartment },
    { defaultKeymap, history, historyKeymap, indentWithTab },
    { searchKeymap, highlightSelectionMatches },
    { closeBrackets, closeBracketsKeymap, autocompletion },
    { bracketMatching, foldGutter, foldKeymap, indentOnInput, syntaxHighlighting, defaultHighlightStyle },
  ] = await Promise.all([
    import('@codemirror/view'),
    import('@codemirror/state'),
    import('@codemirror/commands'),
    import('@codemirror/search'),
    import('@codemirror/autocomplete'),
    import('@codemirror/language'),
  ])

  const languageExtension = await loadLanguage(props.language)

  const theme = View.theme({
    '&': {
      height: '100%',
      fontSize: '13px',
      backgroundColor: 'transparent',
      color: 'var(--text-primary)',
    },
    '.cm-content': {
      fontFamily: 'var(--font-mono)',
      padding: '8px 0',
      caretColor: 'var(--text-primary)',
    },
    '.cm-gutters': {
      backgroundColor: 'transparent',
      color: 'var(--text-muted)',
      border: 'none',
      fontSize: '11px',
    },
    '.cm-activeLine': { backgroundColor: 'var(--surface-hover)' },
    '.cm-activeLineGutter': { backgroundColor: 'transparent', color: 'var(--text-secondary)' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': {
      backgroundColor: 'var(--accent-muted)',
    },
    '.cm-cursor': { borderLeftColor: 'var(--text-primary)' },
    '&.cm-focused': { outline: 'none' },
    '.cm-scroller': { overflow: 'auto', lineHeight: '1.6' },
    '.cm-placeholder': { color: 'var(--text-muted)' },
  })

  const editable = new Compartment()

  const state = EditorState.create({
    doc: props.modelValue,
    extensions: [
      lineNumbers(),
      foldGutter(),
      history(),
      bracketMatching(),
      closeBrackets(),
      autocompletion(),
      indentOnInput(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
      keymap.of([
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...historyKeymap,
        ...searchKeymap,
        ...foldKeymap,
        indentWithTab,
      ]),
      View.lineWrapping,
      placeholderExt(props.placeholder),
      theme,
      editable.of(EditorState.readOnly.of(props.readonly)),
      ...(languageExtension ? [languageExtension] : []),
      View.updateListener.of((update) => {
        if (update.docChanged) emit('update:modelValue', update.state.doc.toString())
      }),
    ],
  })

  view.value = new View({ state, parent: host.value })
  ready.value = true
}

async function loadLanguage (language: string) {
  switch (language) {
    case 'json':
      return (await import('@codemirror/lang-json')).json()
    case 'xml':
      return (await import('@codemirror/lang-xml')).xml()
    case 'javascript':
      return (await import('@codemirror/lang-javascript')).javascript()
    default:
      return null
  }
}

// An external change (loading a different request into the same tab) must
// replace the document without clobbering the cursor on every keystroke.
watch(
  () => props.modelValue,
  (next) => {
    const editor = view.value
    if (!editor) return
    const current = editor.state.doc.toString()
    if (current === next) return
    editor.dispatch({ changes: { from: 0, to: current.length, insert: next } })
  },
)

onMounted(() => void mount())

onBeforeUnmount(() => {
  view.value?.destroy()
  view.value = null
})
</script>

<template>
  <div class="relative h-full min-h-0 w-full overflow-hidden">
    <div ref="host" class="h-full w-full" />
    <textarea
      v-if="!ready"
      class="input-mono absolute inset-0 h-full w-full resize-none border-0 bg-transparent
             p-2 outline-none"
      :value="modelValue"
      :readonly="readonly"
      :placeholder="placeholder"
      spellcheck="false"
      @input="emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)"
    />
  </div>
</template>
