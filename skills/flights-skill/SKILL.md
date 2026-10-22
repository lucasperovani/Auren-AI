# Flight Monitoring Skill

Monitor flight prices and prepare bookings. **Monitoring is separated from purchasing.**

## When to use

- The user asks to track flight prices (for example, "check every weekday at 9 AM whether the flight price has dropped below R$1,000").
- The user asks to buy a ticket after a price condition is met.

## Tools

| Tool | Risk | Behavior |
|---|---|---|
| `flights_search_itineraries` | low | Search through the configured provider |
| `flights_get_price` | low | Current price of an itinerary |
| `flights_monitor_price` | low | Record a price condition for the scheduler |
| `flights_prepare_booking` | medium | Validate passengers and prepare for approval |
| `flights_complete_purchase` | critical | Complete the purchase; **always requires explicit approval** |

## Rules

- **Never invent travel details.** Departure airport, destination, dates, passenger details, baggage, and payment method must come from the user. Ask before enabling the purchase action.
- The purchase approval must identify the itinerary, passenger, final price, currency, taxes, and fees.
- If the price or booking details change after approval, the approval is invalidated and a new one is required.
- A general request to buy a ticket never bypasses the financial safeguards.
- If no travel provider is configured, monitoring and notification still work; purchasing does not.
- Payment credentials are never stored by this skill.

## Requirements

A travel provider API configured in Settings (`flights_api_base`). Without one, only the monitoring condition can be recorded.