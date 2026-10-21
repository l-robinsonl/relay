import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import RelayLogo from './RelayLogo.vue'

describe('RelayLogo', () => {
  it('draws the mark alone by default', () => {
    const wrapper = mount(RelayLogo)
    expect(wrapper.find('svg').exists()).toBe(true)
    expect(wrapper.text()).toBe('')
  })

  it('adds the name when asked', () => {
    expect(mount(RelayLogo, { props: { wordmark: true } }).text()).toBe('Relay')
  })

  it('draws two chevrons, not one', () => {
    // A single chevron is a shell prompt; the pair is what says "relay".
    const wrapper = mount(RelayLogo)
    expect(wrapper.findAll('polyline')).toHaveLength(2)
  })

  it('omits the background unless boxed', () => {
    expect(mount(RelayLogo).find('rect').exists()).toBe(false)
    expect(mount(RelayLogo, { props: { boxed: true } }).find('rect').exists()).toBe(true)
  })

  it('takes the theme colour when unboxed, and white on the box', () => {
    // Unboxed it sits on whatever surface it is placed on, so it must
    // follow the theme rather than being baked blue.
    const plain = mount(RelayLogo)
    expect(plain.find('g').attributes('stroke')).toBe('currentColor')

    const boxed = mount(RelayLogo, { props: { boxed: true } })
    expect(boxed.find('g').attributes('stroke')).toBe('#fff')
  })

  it('scales', () => {
    const wrapper = mount(RelayLogo, { props: { size: 48 } })
    const svg = wrapper.find('svg')
    expect(svg.attributes('width')).toBe('48')
    // The viewBox is fixed so the geometry never distorts.
    expect(svg.attributes('viewBox')).toBe('0 0 24 24')
  })

  it('is hidden from screen readers, since the name is text beside it', () => {
    expect(mount(RelayLogo).find('svg').attributes('aria-hidden')).toBe('true')
  })
})
