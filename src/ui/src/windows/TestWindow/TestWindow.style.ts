import * as stylex from '@stylexjs/stylex';

import { color, font, radius, space, statusPage } from '@/theme/tokens.stylex';

export const styles = stylex.create({
  page: {
    boxSizing: 'border-box',
    minHeight: '100vh',
    margin: 0,
    paddingBlock: space.xxxl,
    paddingInline: space.xxl,
    fontFamily: font.familySystem,
    fontSize: statusPage.fontSize,
    lineHeight: statusPage.lineHeight,
    color: color.text,
    backgroundColor: color.surface,
  },
  content: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.xxl,
    maxWidth: statusPage.maxWidth,
    marginBlock: 0,
    marginInline: 'auto',
  },
  header: {
    display: 'flex',
    flexWrap: 'wrap',
    gap: space.xl,
    alignItems: 'baseline',
    justifyContent: 'space-between',
    paddingBottom: space.xl,
    borderBottomColor: color.divider,
    borderBottomStyle: 'solid',
    borderBottomWidth: '1px',
  },
  title: {
    margin: 0,
    fontSize: statusPage.titleSize,
    fontWeight: statusPage.titleWeight,
    color: color.text,
  },
  subtitle: {
    marginInline: 0,
    marginTop: space.xs,
    marginBottom: 0,
    fontSize: font.sizeMd,
    color: color.textMuted,
  },
  identity: {
    fontFamily: font.familySystemMono,
    fontSize: font.sizeSm,
    color: color.textFaint,
  },
  note: {
    marginInline: 0,
    marginTop: space.lg,
    marginBottom: 0,
    fontSize: font.sizeMd,
    color: color.textMuted,
  },
  summary: {
    display: 'flex',
    flexDirection: 'column',
    gap: space.md,
  },
  readiness: {
    margin: 0,
    fontSize: font.sizeLg,
    fontWeight: statusPage.titleWeight,
    color: color.warning,
  },
  ready: {
    color: color.success,
  },
  alert: {
    marginInline: 0,
    marginTop: space.lg,
    marginBottom: 0,
    paddingBlock: space.md,
    paddingInline: space.lg,
    borderLeftColor: color.danger,
    borderLeftStyle: 'solid',
    borderLeftWidth: statusPage.ruleWidth,
    borderRadius: radius.sm,
    color: color.text,
    backgroundColor: color.divider,
  },
  alertTitle: {
    marginInline: 0,
    marginTop: 0,
    marginBottom: space.xs,
    fontWeight: statusPage.titleWeight,
    color: color.danger,
  },
  alertText: {
    margin: 0,
  },
});
