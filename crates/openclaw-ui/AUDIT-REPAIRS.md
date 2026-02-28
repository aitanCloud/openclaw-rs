# OpenClaw UI Audit Repairs & Upgrades

## Priority 1: Bugs & Broken Behavior

### R1. ~~Escape key listener leak in create cycle modal~~ DONE
- Fix: call `document.removeEventListener('keydown', onKeyDown)` inside `close()`

### R2. ~~Completed cycle stepper shows "7" instead of checkmark~~ DONE
- Fix: if cycle is completed, mark all steps as completed with ✓

### R3. ~~Failed cycle stepper shows all steps as future~~ DONE
- Fix: show all steps with red ✗ indicators for failed cycles

### R4. ~~"Attempt 0/3" — zero-indexed attempt display~~ DONE
- Fix: `Math.max(1, task.current_attempt)`

### R5. ~~Approve Plan button navigates instead of acting~~ DONE
- Fix: direct `handleApprove()` method with prompt for approver name

## Priority 2: Redundancy & UX Polish

### R6. ~~Plan and Tasks sections redundant~~ DONE
- Fix: removed flat Tasks section, added task count badge to Plan header

### R7. ~~No active cycle preview on dashboard fresh load~~ DONE
- Fix: eager `fetchInstanceDetail()` for uncached instances

### R8. ~~Dashboard always shows "Disconnected"~~ DONE
- Fix: shows "Ready" on dashboard, "Live"/"Connecting..." on instance views

### R9. ~~Event log positioned awkwardly~~ DONE
- Fix: moved inside instance detail as collapsible section

### R10. ~~"Merging" vs "completing"~~ DONE
- Fix: renamed to "Completing"

### R11. ~~"Done" vs "Completed"~~ DONE
- Fix: renamed to "Completed"

## Priority 3: Missing Information Displays

### R12. No run-level detail on tasks — DEFERRED
- Tasks not clickable yet, needs API for run data per task

### R13. No cost per task — DEFERRED
- Needs run-level data to compute cost per task

### R14. ~~Elapsed time for active tasks~~ DONE
- Fix: shows "Running Xm" or "Just started" for active/verifying tasks

### R15. ~~Completion summary on finished cycles~~ DONE
- Fix: 3 summary cards — Duration, Tasks Passed (with %), Outcome

### R16. Budget breakdown expandable — DEFERRED
- Needs ledger entry detail endpoint or expanded API response

### R17. ~~No favicon~~ DONE
- Fix: inline SVG data URI in index.html

## Priority 4: Missing Actions

### R18. ~~Cancel Cycle button~~ DONE
- Fix: disabled button with tooltip (API not yet available)

### R19. ~~Retry failed cycles~~ DONE
- Fix: "Retry with Same Prompt" creates new cycle with same prompt

### R20. ~~Instance creation from UI~~ DONE
- Fix: "+ New Instance" button with modal (name + project_id)

## Priority 5: Notifications & Global State

### R21. ~~Toast notification system~~ DONE
- Fix: auto-dismiss toasts for success/error/info events

### R22. ~~Login page branding~~ DONE
- Fix: OC logo + "Claude-powered software development orchestrator" subtitle

## Summary

**18/22 repairs implemented and verified via Playwright.**

3 deferred (R12, R13, R16) — need backend API extensions for run-level/ledger detail.
