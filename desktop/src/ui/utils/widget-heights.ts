/**
 * Last measured height per widget, so a remounted frame doesn't jump. Kept for
 * the most recently seen widgets only (a Map in least-recent-first order); one
 * that falls out re-measures from the initial height when it next mounts.
 */
export const KNOWN_HEIGHTS_MAX = 500;
const knownHeights = new Map<string, number>();

export function rememberedHeight(toolUseId: string): number | undefined {
  const height = knownHeights.get(toolUseId);
  if (height !== undefined) rememberHeight(toolUseId, height);
  return height;
}

export function rememberHeight(toolUseId: string, height: number): void {
  knownHeights.delete(toolUseId);
  knownHeights.set(toolUseId, height);
  for (const oldest of knownHeights.keys()) {
    if (knownHeights.size <= KNOWN_HEIGHTS_MAX) break;
    knownHeights.delete(oldest);
  }
}
