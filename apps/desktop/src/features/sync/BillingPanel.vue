<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { CreditCard, ExternalLink, Loader2, TriangleAlert } from '@lucide/vue'
import type { BillingView, PlanOffer } from '@relay/shared-types'
import { api } from '@/services/ipc'

const props = defineProps<{ workspaceId: string, open: boolean }>()

const billing = ref<BillingView | null>(null)
const loading = ref(false)
const working = ref(false)
const error = ref<string | null>(null)
/** The link that was opened, offered again in case nothing appeared. */
const openedUrl = ref<string | null>(null)

function messageOf (value: unknown): string {
  if (value && typeof value === 'object' && 'message' in value) {
    const detail = 'detail' in value ? (value as { detail?: string | null }).detail : null
    const message = String((value as { message: unknown }).message)
    return detail ? `${message} — ${detail}` : message
  }
  return String(value)
}

async function load () {
  loading.value = true
  try {
    billing.value = await api.syncBilling(props.workspaceId)
    error.value = null
  } catch (e) {
    // A server too old to know the route, or one that is simply down.
    // Either way the panel hides itself rather than showing a red box
    // about money the user may not owe.
    billing.value = null
    error.value = messageOf(e)
  } finally {
    loading.value = false
  }
}

// Payment happens in a browser, so nothing tells the app when it is
// done. After a checkout is opened the panel checks back for a couple
// of minutes — long enough for a card to clear and the webhook to
// land, short enough not to poll a server for ever.
let poll: ReturnType<typeof setInterval> | undefined
let pollsLeft = 0

function stopPolling () {
  clearInterval(poll)
  poll = undefined
  pollsLeft = 0
}

function pollForTheResult () {
  stopPolling()
  pollsLeft = 24
  poll = setInterval(async () => {
    if (!props.open || pollsLeft-- <= 0) {
      stopPolling()
      return
    }
    const before = billing.value?.hasSubscription
    await load()
    if (billing.value?.hasSubscription !== before) stopPolling()
  }, 5000)
}

onBeforeUnmount(stopPolling)

watch(() => [props.open, props.workspaceId], ([isOpen]) => {
  if (isOpen) load()
  else stopPolling()
}, { immediate: true })

/** Seats are only a limit when the server says it charges. */
const metered = computed(() => billing.value?.metered === true)

const seatLine = computed(() => {
  const view = billing.value
  if (!view) return ''
  if (view.seats == null) return `${view.members} ${view.members === 1 ? 'person' : 'people'}`
  return `${view.members} of ${view.seats} ${view.seats === 1 ? 'seat' : 'seats'}`
})

const needsMoreSeats = computed(() => {
  const view = billing.value
  return view?.seats != null && view.members > view.seats
})

/**
 * Which plan the person has picked, before there is a subscription.
 *
 * Null until the panel loads, then the first offer — which the
 * server orders cheapest first, so the default is never the dearest
 * thing on the list.
 */
const chosenPlan = ref<string | null>(null)

watch(billing, (view) => {
  if (!chosenPlan.value && view?.plans?.length) {
    chosenPlan.value = view.plans[0]!.id
  }
})

/**
 * A plan's price, in the words a person uses.
 *
 * Both figures for a yearly plan: the amount that leaves the account
 * and the monthly equivalent, because the second is the one being
 * compared and the first is the one being charged.
 */
function priceOf (plan: PlanOffer): string {
  const money = (minor: number) =>
    new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: plan.currency.toUpperCase(),
    }).format(minor / 100)

  const each = `${money(plan.amount)} per person`
  if (plan.interval !== 'year') return `${each} a month`
  return `${each} a year — ${money(Math.round(plan.amount / 12))} a month`
}

async function buySeats () {
  working.value = true
  error.value = null
  try {
    // No seat count: the server buys one for every member, which is
    // the only count that lets everyone keep working.
    openedUrl.value = await api.syncBillingCheckout(
      props.workspaceId,
      null,
      chosenPlan.value,
    )
    pollForTheResult()
  } catch (e) {
    error.value = messageOf(e)
  } finally {
    working.value = false
  }
}

/**
 * Whether the button below starts a fresh checkout.
 *
 * Not simply "has no subscription": a cancelled one is still a
 * subscription and still falls through to a checkout, so a renewal
 * must be offered the same choice as a first purchase. Tied to the
 * same condition the action uses, because the two disagreeing is how
 * somebody ends up buying a plan they were never shown.
 */
const buying = computed(() => {
  const current = billing.value
  return !!current && !(current.hasSubscription && current.active)
})

/** The one button, and what it does. */
const action = computed(() => {
  const current = billing.value
  if (!current) return null

  // The portal manages a *live* subscription. A cancelled one has
  // nothing there to manage, so starting again is a fresh checkout.
  if (current.hasSubscription && current.active) {
    return {
      // Matching the warning above it: being told buying more seats
      // clears the problem, under a button saying "Manage billing",
      // leaves you guessing which screen does that.
      label: needsMoreSeats.value ? 'Add seats' : 'Manage billing',
      run: manage,
    }
  }

  return {
    label: current.members === 1 ? 'Buy a seat' : `Buy ${current.members} seats`,
    run: buySeats,
  }
})

async function manage () {
  working.value = true
  error.value = null
  try {
    openedUrl.value = await api.syncBillingPortal(props.workspaceId)
    pollForTheResult()
  } catch (e) {
    error.value = messageOf(e)
  } finally {
    working.value = false
  }
}
</script>

<template>
  <!-- Nothing at all on a server that does not charge. Self-hosting is
       the point of Relay; a greyed-out billing box would suggest it is
       a trial of something. -->
  <div v-if="metered && billing" class="space-y-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class="text-xs font-medium text-secondary">Seats</h3>
      <span class="text-xs text-muted">{{ seatLine }}</span>
    </div>

    <p
      v-if="billing.notice"
      class="text-xs"
      :style="{ color: billing.active ? 'var(--warning)' : 'var(--danger)' }"
    >
      {{ billing.notice }}
    </p>
    <p v-else-if="needsMoreSeats" class="text-xs" :style="{ color: 'var(--warning)' }">
      There are more people here than seats paid for. Nobody is locked out; buying the
      difference clears it.
    </p>

    <!-- Only when the button below buys something. Changing plan on
         a live subscription is Stripe's portal's job, where the
         proration is worked out; offering it here would start a
         second subscription beside the first. A cancelled one is not
         live, so renewing gets the choice. -->
    <fieldset
      v-if="billing.canManage && buying && (billing.plans?.length ?? 0) > 1"
      class="space-y-1"
    >
      <legend class="mb-1 text-xs text-muted">What to buy</legend>
      <label
        v-for="plan in billing.plans"
        :key="plan.id"
        class="flex cursor-pointer items-start gap-2 rounded border p-2 text-xs"
        :style="{
          borderColor: chosenPlan === plan.id ? 'var(--accent)' : 'var(--border-subtle)',
          backgroundColor: chosenPlan === plan.id ? 'var(--accent-muted)' : 'transparent',
        }"
      >
        <input
          v-model="chosenPlan"
          type="radio"
          :value="plan.id"
          name="relay-plan"
          class="mt-0.5 shrink-0"
        />
        <span class="min-w-0">
          <span class="block text-primary">{{ plan.label }}</span>
          <span class="block text-muted">{{ priceOf(plan) }}</span>
        </span>
      </label>
    </fieldset>

    <div v-if="billing.canManage" class="flex gap-2">
      <button
        type="button"
        class="btn btn-primary flex-1 gap-1.5 text-xs"
        :disabled="working"
        @click="action?.run()"
      >
        <Loader2 v-if="working" :size="13" class="animate-spin" />
        <CreditCard v-else :size="13" />
        {{ action?.label }}
      </button>
      <button type="button" class="btn text-xs" :disabled="loading" @click="load">
        Refresh
      </button>
    </div>
    <p v-else class="text-xs text-muted">
      Only an administrator of this workspace can change the subscription.
    </p>

    <p v-if="openedUrl" class="text-xs text-muted">
      Opened in your browser.
      <a
        :href="openedUrl"
        target="_blank"
        rel="noreferrer noopener"
        class="inline-flex items-center gap-1 underline"
      >
        Open it again
        <ExternalLink :size="11" />
      </a>
    </p>

    <!-- Only before the first payment. That is when someone wonders
         whether to trust a desktop app with a card; afterwards they
         know, and in a 320px panel it is two lines of permanent
         boilerplate — including, until now, under "you are not an
         administrator", where there is nothing to pay with. -->
    <p v-if="billing.canManage && !billing.hasSubscription" class="text-xs text-muted">
      Payment is handled by Stripe, in your browser. Relay never sees a card number.
    </p>

    <div
      v-if="error"
      class="flex items-start gap-2 rounded border p-2.5 text-xs"
      :style="{
        borderColor: 'var(--danger)',
        backgroundColor: 'var(--danger-muted)',
        color: 'var(--danger)',
      }"
    >
      <TriangleAlert :size="13" class="mt-px shrink-0" />
      <span>{{ error }}</span>
    </div>
  </div>
</template>
