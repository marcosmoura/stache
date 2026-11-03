export interface WindowFrame {
  x: number;
  y: number;
  width: number;
  height: number;
  screenWidth: number;
  screenHeight: number;
}

export interface Size {
  width: number;
  height: number;
}

export interface Rect extends Size {
  x: number;
  y: number;
}

export type WidgetNames = 'calendar' | 'battery' | 'weather';

export interface WidgetConfig {
  name: WidgetNames;
  rect: Rect;
}
