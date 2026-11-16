// "Support the creator" reminder: when it is due.

/** True at the 10th launch, then every 100 launches, unless opted out. */
export function supportDue(count: number, optOut: boolean): boolean {
  if (optOut) return false;
  return count === 10 || (count >= 100 && count % 100 === 0);
}
