// Runs the layout editor's generator functions (assets/editor-core.js, the same file the
// page loads) outside a browser:
//
//   node scripts/editor_core_check.js <geometry.json> <layout.json>
//
// with the JSON of GET /api/geometry and GET /api/layout. Prints one JSON object: the
// guessed generators, the verification against them (base = first set key), what "Repair"
// changes, and the repaired layout. Used by the tests in src/edit.rs.

const fs = require('fs');
const path = require('path');
const core = require(path.join(__dirname, '..', 'assets', 'editor-core.js'));

const [geometryFile, layoutFile] = process.argv.slice(2);
const geometry = JSON.parse(fs.readFileSync(geometryFile, 'utf8'));
const layout = JSON.parse(fs.readFileSync(layoutFile, 'utf8'));

const guess = core.guess(geometry, layout);
if (guess.error) {
  console.log(JSON.stringify({ error: guess.error }));
  process.exit(0);
}
// what the page puts into the generator inputs after "Guess from layout"
const gen = {
  gA: guess.gA,
  gB: guess.gB,
  base: { board: guess.firstSet.board, key: guess.firstSet.key },
  baseKey: guess.firstSet.entry.key,
  baseChan: guess.firstSet.entry.chan,
};
const verify = core.verify(geometry, layout, gen);
const repaired = JSON.parse(JSON.stringify(layout));
const repair = core.repair(geometry, repaired, gen, guess.palette);
const after = core.verify(geometry, repaired, gen);

console.log(JSON.stringify({
  basis: core.basis(geometry),
  guess: { gA: guess.gA, gB: guess.gB, fitPct: guess.fitPct },
  verify,
  repair,
  after: { totalSet: after.totalSet, mismatch: after.mismatch.length, missing: after.missing.length },
  layout: repaired,
}));
