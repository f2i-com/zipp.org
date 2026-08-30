// Reduced from a poker table's betting round. Two shapes worth keeping: the
// side-pot loop, whose nesting comes from the rules themselves (per pot, per
// eligible player, per hand category), and the hand ranker, which scores a hand
// as one weighted sum of its categories so the ordering lives in one expression
// instead of a comparison chain nobody can audit.

function settleShowdown(table) {
  var pots = buildSidePots(table.players, table.bets);
  for (var p = 0; p < pots.length; p = p + 1) {
    var pot = pots[p];
    var best = -1;
    var winners = [];
    for (var s = 0; s < pot.eligible.length; s = s + 1) {
      var seat = table.players[pot.eligible[s]];
      if (seat.folded) { continue; }
      try {
        var score = rankHand(seat.hole.concat(table.board));
        if (score > best) {
          best = score;
          winners = [seat];
        } else if (score === best) {
          if (seat.allIn) {
            if (winners.length > 0 && winners[0].allIn) { winners.push(seat); }
            else { winners = [seat]; }
          } else {
            winners.push(seat);
          }
        }
      } catch (err) {
        table.log.push("hand " + seat.id + " unrankable: " + err.message);
      }
    }
    var share = Math.floor(pot.amount / (winners.length || 1));
    for (var w = 0; w < winners.length; w = w + 1) {
      winners[w].stack = winners[w].stack + share;
    }
  }
}

// One expression, seventeen operands: every category weighted so that a higher
// category always outranks any combination of lower ones. Splitting it into a
// running total would let a weight and its category drift apart, which is
// exactly the bug this form prevents.
function rankHand(cards) {
  var c = categorise(cards);
  return c.straightFlush * 0x10000000 +
    c.quads * 0x1000000 +
    c.fullHouse * 0x400000 +
    c.flush * 0x100000 +
    c.straight * 0x40000 +
    c.trips * 0x10000 +
    c.twoPair * 0x8000 +
    c.pair * 0x2000 +
    c.kicker0 * 0x400 +
    c.kicker1 * 0x100 +
    c.kicker2 * 0x40 +
    c.kicker3 * 0x10 +
    c.kicker4 * 0x4 +
    c.suitBreak * 0x2 +
    c.lowBall +
    c.wheelPenalty +
    c.deadCardAdjust;
}

function seatLabel(table, index) {
  return table.view.seats.rows[index].nameplate.text.value.trim().toUpperCase();
}
