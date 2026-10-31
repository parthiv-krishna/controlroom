// The committed development fixture, typed for tests.
import raw from '../../fixtures/controlroom.json';
import type { Fixture } from '../lib/api/fixture';

/** A fresh deep copy of web/fixtures/controlroom.json. */
// SAFETY: JSON imports widen string unions; fixture.test.ts checks the fixture's consistency.
export const loadFixture = (): Fixture => structuredClone(raw) as unknown as Fixture;
