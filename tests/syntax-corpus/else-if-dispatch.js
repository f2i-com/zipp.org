// Reduced from an adventure game's hotspot dispatcher: one interaction handler
// per clickable thing in the world, written as a single `else if` ladder so the
// reading order matches the order a player meets them.
//
// This is the shape that broke first. Each `else if` is another `if` statement
// in the alternate branch of the one before it, so a ladder of N arms is a tree
// N deep — one recursion level and one AST level per arm, on top of whatever
// the arms themselves contain. The v0.0.1 budget of 48 was reached at arm 26.
//
// A dispatcher is the honest form here: the arms share the guard clauses above
// them and mutate the same world state, so a lookup table of closures would
// need every one of them to take the same arguments and would move the reading
// order into a data structure.

function handleHotspot(id) {
  if (choiceList.length > 0) { return }
  if (dlgActive) {
    fastForwardDialogue()
    if (choiceList.length > 0 || dlgActive) { return }
  }
  if (id === "hs_coffee") {
    if (!coffeeFixed) {
      coffeeFixed = true
      sfx("success")
      addLog("FIXED: coffee machine firmware, rolled back with prejudice")
    } else { showNote("The coffee machine hums gratefully.") }
  } else if (id === "hs_board") {
    if (!debugDone) { debugOpen = true; debugPicked = [] }
    else { showNote("The incident board is calm. Suspiciously calm.") }
  } else if (id === "hs_desk41") {
    if (!notebookSeen) { notebookSeen = true; addLog("FOUND: a notebook on desk 41") }
    else { showNote("Desk 41. Cleared like it was never anyone's.") }
  } else if (id === "hs_meeting") {
    if (tasksDone() >= 2) { enterMeeting() }
    else if (!cliveWarned) { cliveWarned = true }
    else { showNote("Stand-up can wait ninety more seconds.") }
  } else if (id === "hs_laptop") {
    if (!budgetOpen) { budgetOpen = true }
  } else if (id === "hs_phone") {
    if (phoneRinging) { phoneRinging = false; buildFlat() }
    else { showNote("No new messages.") }
  } else if (id === "hs_luma") {
    if (productName !== "") { showNote("Idle, and for once not offering to help.") }
    else if (budgetDone) { enterLuma() }
    else { showNote("You already know what the dream costs.") }
  } else if (id === "hs_tabA" || id === "hs_tabB") {
    if (bakeryDay) { showNote("The tablets are quiet.") }
    else if (!ordersDone) { openOrders() }
    else { showNote("The channels agree with each other now.") }
  } else if (id === "hs_paper") {
    if (bakeryDay) {
      if (!wfDone) { wfOpen = true }
      else { showNote("Six policies, pinned where the flour can reach them.") }
    }
    else if (!ordersDone) { openOrders() }
    else { showNote("Historic.") }
  } else if (id === "hs_mabel") {
    if (bakeryDay) { showNote(wfDone ? "Tuesday, says Mabel." : "Mabel is elbow-deep in the rush.") }
    else if (!ordersDone) { showNote("Leo waves you toward the tablets.") }
    else if (!pilotPromised) { runDialogue(["mabel_053"], "bakery_pitch") }
    else { showNote("Mabel points at the door, then at an imaginary watch.") }
  } else if (id === "hs_exit") {
    if (bakeryDay) {
      if (wfDone) { endSlice() }
      else { showNote("Not before the policies are set.") }
    }
    else if (pilotPromised) { goDream() }
    else { showNote("Leaving now would waste the best accident of the week.") }
  } else if (id === "hs_priya") {
    if (priyaHere && priyaMode === "") { runDialogue(["priya_080", "alex_082"], "collab") }
    else if (priyaMode !== "" && !scopeDone) { scopeOpen = true }
    else { showNote("She has opinions. They can wait.") }
  } else if (id === "hs_abox") {
    if (picked.length >= 3) { confirmItems() }
    else { showNote("The box holds three things. You have " + picked.length + ".") }
  } else if (id.indexOf("bit_") === 0) {
    var key = id.slice(4)
    toggleItem(key)
    showNote(itemName(key) + " goes back on the desk.")
  } else if (id === "it_doug" || id === "it_plant" || id === "it_photo") {
    var item = id.slice(3)
    if (toggleItem(item)) { showNote(itemName(item) + " - " + itemDesc(item)) }
  } else if (id === "hs_kiosk") {
    showNote("The wellbeing kiosk offers a breathing exercise. It requires SSO.")
  } else if (id === "plant1_top") {
    showNote("The plant has an asset tag and a quarterly growth objective.")
  } else if (id === "lobby_banner") {
    showNote("HUMAN POTENTIAL WEEK. The T flickers like it knows something.")
  } else if (id === "mcol0_t") {
    showNote("They're recording the meeting. Phone at 3%. Solidarity.")
  } else if (id === "mcol2_t") {
    showNote("Their beach screensaver has more agency than anyone in this room.")
  } else if (id === "hs_whiteboard") {
    showNote("Someone has drawn a roadmap. Someone else has drawn a shark.")
  } else if (id === "hs_printer") {
    showNote("The printer is jammed in a way that predates the building.")
  } else if (id === "hs_fridge") {
    showNote("A labelled lunch, dated optimistically.")
  } else if (id === "hs_server") {
    showNote("The rack breathes out. Somewhere in there, your commit is running.")
  } else if (id === "hs_window") {
    showNote("Rain, and a car park that has never once been full.")
  } else if (id === "hs_door") {
    // The deepest arm in the file, and deliberately so: the arm at the bottom
    // of the ladder already sits N levels down, and whatever it contains nests
    // on top of that. This is where the AST-nesting budget is actually spent.
    if (bakeryDay) {
      if (wfDone) {
        if (pilotPromised) {
          if (picked.length >= 3) {
            if (scopeDone) { endSlice() }
            else { showNote(priyaHere ? "Priya is still reading." : "Scope first.") }
          } else { showNote("PAX is counting.") }
        } else { showNote("One week, she said.") }
      } else { showNote("The board is watching.") }
    } else { showNote("The door is shut and the day is not.") }
  } else {
    showNote("Nothing here wants anything from you.")
  }
}
