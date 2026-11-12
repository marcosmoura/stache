import * as stylex from '@stylexjs/stylex';

import { color, font, radius, space, statusPage } from '@/theme/tokens.stylex';

/** The test window's section panels, lists and cards, shared by its lists. */
export const card = stylex.create({
  section: {
    padding: space.xl,
    borderColor: color.divider,
    borderStyle: 'solid',
    borderWidth: '1px',
    borderRadius: statusPage.cardRadius,
    backgroundColor: color.surfaceRaised,
  },
  sectionTitle: {
    marginInline: 0,
    marginTop: 0,
    marginBottom: space.lg,
    fontSize: statusPage.fontSize,
    fontWeight: statusPage.titleWeight,
    color: color.textSecondary,
  },
  empty: {
    margin: 0,
    fontSize: font.sizeMd,
    color: color.textMuted,
  },
  list: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.lg,
    margin: 0,
    padding: 0,
    listStyle: 'none',
  },
  card: {
    paddingBlock: space.lg,
    paddingInline: space.xl,
    borderColor: color.divider,
    borderStyle: 'solid',
    borderWidth: '1px',
    borderRadius: statusPage.cardRadius,
    backgroundColor: color.surface,
  },
  heading: {
    display: 'flex',
    gap: space.md,
    alignItems: 'center',
    justifyContent: 'space-between',
  },
  title: {
    margin: 0,
    fontFamily: font.familySystemMono,
    fontSize: font.sizeMd,
    fontWeight: statusPage.titleWeight,
    color: color.text,
  },
  muted: {
    marginInline: 0,
    marginTop: space.md,
    marginBottom: 0,
    fontSize: font.sizeMd,
    color: color.textMuted,
  },
  /** A labelled block with a colored leading rule (diagnostics, warnings). */
  callout: {
    marginInline: 0,
    marginTop: space.lg,
    marginBottom: 0,
    paddingBlock: space.md,
    paddingInline: space.lg,
    borderLeftStyle: 'solid',
    borderLeftWidth: statusPage.ruleWidth,
    borderRadius: radius.sm,
    color: color.textSecondary,
    backgroundColor: color.divider,
  },
  diagnostic: { borderLeftColor: color.attention },
  warning: { borderLeftColor: color.warning },
  calloutLabel: {
    marginInline: 0,
    marginTop: 0,
    marginBottom: space.xs,
    fontSize: font.sizeSm,
    fontWeight: statusPage.titleWeight,
    color: color.textMuted,
  },
  calloutText: {
    margin: 0,
  },
  calloutList: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xs,
    margin: 0,
    paddingLeft: space.xl,
  },
});
