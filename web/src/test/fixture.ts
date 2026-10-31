// The committed development fixture, typed for tests.
import raw from '../../fixtures/controlroom.json';
import type { Fixture } from '../lib/api/fixture';

/** A fresh deep copy of web/fixtures/controlroom.json. */
export const loadFixture = (): Fixture => structuredClone(raw) as unknown as Fixture;
