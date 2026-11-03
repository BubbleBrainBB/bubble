// Widget heights remembered across remounts stay bounded: the least recently
// seen widget is forgotten first, and reading a height counts as seeing it.
import assert from 'node:assert/strict';
import { KNOWN_HEIGHTS_MAX, rememberedHeight, rememberHeight } from '../../src/ui/utils/widget-heights';

rememberHeight('first', 120);
rememberHeight('second', 240);
assert.equal(rememberedHeight('first'), 120);
rememberHeight('second', 300);
assert.equal(rememberedHeight('second'), 300, 'a new measurement replaces the old one');
assert.equal(rememberedHeight('missing'), undefined);

// Fill the cache; reading 'first' makes 'second' the least recently seen.
for (let index = 0; index < KNOWN_HEIGHTS_MAX - 2; index += 1) rememberHeight(`filler-${index}`, index + 1);
assert.equal(rememberedHeight('first'), 120, 'full, nothing forgotten yet');
rememberHeight('one-more', 1);
assert.equal(rememberedHeight('second'), undefined, 'the least recently seen widget is forgotten');
assert.equal(rememberedHeight('first'), 120, 'reading kept it recent');
for (let index = 0; index < KNOWN_HEIGHTS_MAX * 3; index += 1) rememberHeight(`flood-${index}`, 1);
assert.equal(rememberedHeight('first'), undefined, 'old widgets give way to new ones');
assert.equal(rememberedHeight(`flood-${KNOWN_HEIGHTS_MAX * 3 - 1}`), 1);

console.log('widget-heights: bounded, least recently seen forgotten first');
