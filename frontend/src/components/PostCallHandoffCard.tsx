'use client';

import type { ReactNode } from 'react';
import { Users } from 'lucide-react';
import { Spinner } from '@/components/ui/spinner';
import { RecordingCardSlot } from '@/components/RecordingCardSlot';
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog';
import { cn } from '@/lib/utils';

/**
 * Stopping and saving use the recording dock. Speaker choices opt into a
 * compact modal; their busy progress can use a corner card without an overlay or
 * focus trap so the meeting remains usable while processing continues.
 */
export function PostCallHandoffCard({
  title,
  detail,
  busy = false,
  icon,
  children,
  centered = false,
  placement = 'dock',
  onDismiss,
}: {
  title: string;
  detail?: string;
  busy?: boolean;
  icon?: ReactNode;
  children?: ReactNode;
  /** Center choices/errors; busy progress stays compact in the nonmodal dock. */
  centered?: boolean;
  /** Background transcript progress can sit outside the recording dock. */
  placement?: 'dock' | 'top-left';
  onDismiss?: () => void;
}) {
  if (centered && !busy) {
    return (
      <Dialog open onOpenChange={(open) => { if (!open) onDismiss?.(); }}>
        <DialogContent
          className="max-w-sm gap-4 p-5"
          showCloseButton={!!onDismiss}
          onEscapeKeyDown={(event) => { if (!onDismiss) event.preventDefault(); }}
          onPointerDownOutside={(event) => event.preventDefault()}
        >
          <div className="flex items-start gap-3">
            <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-af-accent/[0.12] text-af-accent">
              {busy ? <Spinner size={18} /> : icon ?? <Users size={18} strokeWidth={1.75} />}
            </span>
            <div className="min-w-0">
              <DialogTitle className="text-sm">{title}</DialogTitle>
              {detail && <DialogDescription className="mt-1 text-xs">{detail}</DialogDescription>}
            </div>
          </div>
          {children}
        </DialogContent>
      </Dialog>
    );
  }
  const compact = placement === 'top-left';
  const content = (
      <div className={cn(
        'pointer-events-auto w-full animate-af-rise border border-af-border-strong bg-af-elevated/95 text-af-text shadow-xl backdrop-blur-xl',
        compact ? 'rounded-xl px-3 py-3' : 'rounded-[26px] px-5 py-4',
        !compact && (centered ? 'max-w-sm' : 'max-w-[36rem]'),
      )}>
        <div className={cn('flex items-center', compact ? 'gap-2.5' : 'gap-3')}>
          <span className={cn('flex shrink-0 items-center justify-center rounded-full bg-af-accent/[0.12] text-af-accent', compact ? 'h-8 w-8' : 'h-12 w-12')}>
            {busy ? <Spinner size={compact ? 14 : 18} /> : icon ?? <Users size={18} strokeWidth={1.75} />}
          </span>
          <div className="min-w-0 text-left leading-tight">
            <div className={cn('font-semibold tracking-tight', compact ? 'text-xs' : 'text-sm')}>{title}</div>
            {detail && <div className={cn('mt-0.5 text-af-text-3', compact ? 'text-[11px] leading-snug' : 'text-xs')}>{detail}</div>}
          </div>
        </div>
        {children && <div className={compact ? 'mt-2.5' : 'mt-4'}>{children}</div>}
      </div>
  );
  return compact ? (
    <div className="pointer-events-none fixed left-4 top-4 z-[70] w-[min(20rem,calc(100vw-2rem))]">
      {content}
    </div>
  ) : <RecordingCardSlot>{content}</RecordingCardSlot>;
}
