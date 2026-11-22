import { useCallback, useEffect, useRef, useState } from 'react';

/**
 * A disclosure whose body is mounted on first open and animates every time.
 *
 * `.vd-expand` animates `grid-template-rows` from `0fr` to `1fr`, which needs
 * the body to exist with `data-open="false"` for one painted frame before the
 * attribute flips. Mounting and opening in the same commit would skip the
 * transition, so the first open arms on the frame after the mount.
 *
 * The body stays mounted after that: a card the user has opened is a card they
 * are looking at, and re-mounting it on every toggle would throw away the
 * transition the mount was for. A card that has never been opened carries no
 * body at all, which is the common case in a long transcript.
 *
 * Two frames, not one: React commits the mount, the browser paints the
 * collapsed state, and only then does the attribute change.
 */
export function useDisclosure() {
  const [open, setOpen] = useState(false);
  const [mounted, setMounted] = useState(false);
  const [armed, setArmed] = useState(false);
  const armFrame = useRef(0);
  const armInner = useRef(0);

  // A card can leave the transcript between the click and the frame that arms
  // it. Cancel both pending frames so the arm never lands on a dead component.
  useEffect(
    () => () => {
      cancelAnimationFrame(armFrame.current);
      cancelAnimationFrame(armInner.current);
    },
    [],
  );

  const toggle = useCallback(() => {
    const next = !open;
    setOpen(next);
    if (next && !mounted) {
      setMounted(true);
      armFrame.current = requestAnimationFrame(() => {
        armInner.current = requestAnimationFrame(() => setArmed(true));
      });
    }
  }, [open, mounted]);

  return {
    open,
    /** Whether the body should be in the tree at all. */
    mounted,
    /** Whether the body has been painted collapsed once and can transition. */
    armed,
    toggle,
  };
}
