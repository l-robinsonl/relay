import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import { api } from '@/services/ipc'

/** Which way the request and response panes are arranged. */
export type LayoutOrientation = 'horizontal' | 'vertical'

/** Theme and layout preferences, persisted to the local database. */
export const useSettingsStore = defineStore('settings', () => {
  const theme = ref<'dark' | 'light'>('dark')
  const orientation = ref<LayoutOrientation>('horizontal')
  const sidebarWidth = ref(260)
  const responseSize = ref(50)
  const sidebarVisible = ref(true)
  const ready = ref(false)

  function applyTheme () {
    document.documentElement.classList.toggle('light', theme.value === 'light')
    document.documentElement.classList.toggle('dark', theme.value === 'dark')
  }

  async function load () {
    try {
      const [storedTheme, storedLayout] = await Promise.all([
        api.stateGet('theme'),
        api.stateGet('layout'),
      ])

      if (storedTheme === 'light' || storedTheme === 'dark') theme.value = storedTheme

      if (storedLayout) {
        // A corrupted preference must not stop the app from starting.
        try {
          const parsed = JSON.parse(storedLayout) as Partial<{
            orientation: LayoutOrientation
            sidebarWidth: number
            responseSize: number
            sidebarVisible: boolean
          }>
          if (parsed.orientation) orientation.value = parsed.orientation
          if (typeof parsed.sidebarWidth === 'number') {
            sidebarWidth.value = clamp(parsed.sidebarWidth, 180, 560)
          }
          if (typeof parsed.responseSize === 'number') {
            responseSize.value = clamp(parsed.responseSize, 20, 80)
          }
          if (typeof parsed.sidebarVisible === 'boolean') {
            sidebarVisible.value = parsed.sidebarVisible
          }
        } catch {
          // Keep the defaults.
        }
      }
    } finally {
      applyTheme()
      ready.value = true
    }
  }

  async function persistLayout () {
    await api.stateSet(
      'layout',
      JSON.stringify({
        orientation: orientation.value,
        sidebarWidth: sidebarWidth.value,
        responseSize: responseSize.value,
        sidebarVisible: sidebarVisible.value,
      }),
    )
  }

  async function setTheme (next: 'dark' | 'light') {
    theme.value = next
    applyTheme()
    await api.stateSet('theme', next)
  }

  function toggleTheme () {
    return setTheme(theme.value === 'dark' ? 'light' : 'dark')
  }

  function toggleOrientation () {
    orientation.value = orientation.value === 'horizontal' ? 'vertical' : 'horizontal'
    return persistLayout()
  }

  function toggleSidebar () {
    sidebarVisible.value = !sidebarVisible.value
    return persistLayout()
  }

  // Layout changes come from dragging a splitter, which fires continuously;
  // persisting every frame would mean hundreds of writes per drag.
  let saveTimer: ReturnType<typeof setTimeout> | undefined
  watch([sidebarWidth, responseSize], () => {
    if (!ready.value) return
    clearTimeout(saveTimer)
    saveTimer = setTimeout(() => void persistLayout(), 400)
  })

  return {
    theme,
    orientation,
    sidebarWidth,
    responseSize,
    sidebarVisible,
    ready,
    load,
    setTheme,
    toggleTheme,
    toggleOrientation,
    toggleSidebar,
    persistLayout,
  }
})

function clamp (value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max)
}
