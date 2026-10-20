import tv from '../../resources/icons/ui/tv-24.svg?raw';
import minus from '../../resources/icons/ui/minus-16.svg?raw';
import close from '../../resources/icons/ui/close-16.svg?raw';
import refresh from '../../resources/icons/ui/refresh-20.svg?raw';
import more from '../../resources/icons/ui/more-horizontal-16.svg?raw';
import edit from '../../resources/icons/ui/edit-16.svg?raw';
import remove from '../../resources/icons/ui/delete-16.svg?raw';
import chevronRight from '../../resources/icons/ui/chevron-right-20.svg?raw';
import chevronDown from '../../resources/icons/ui/chevron-down-16.svg?raw';
import check from '../../resources/icons/ui/check-24.svg?raw';
import checkSmall from '../../resources/icons/ui/check-16.svg?raw';
import portHdmi from '../../resources/icons/ui/port-hdmi-16.svg?raw';

// These trusted, bundled SVGs inherit CSS color. Prepare decorative markup once;
// the surrounding button or control supplies its accessible name.
const decorative = (svg: string) => svg.replace('<svg ', '<svg aria-hidden="true" focusable="false" ');

export const icons = {
  tv: decorative(tv),
  minus: decorative(minus),
  close: decorative(close),
  refresh: decorative(refresh),
  more: decorative(more),
  edit: decorative(edit),
  delete: decorative(remove),
  chevronRight: decorative(chevronRight),
  chevronDown: decorative(chevronDown),
  check: decorative(check),
  checkSmall: decorative(checkSmall),
  portHdmi: decorative(portHdmi),
} as const;
