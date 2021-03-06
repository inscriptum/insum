import { Define, AbstractElement, state, attr } from 'abstract-element';
import litRender from 'abstract-element/render/lit';
import { html, TemplateResult } from 'lit-html';

import './search-icon';

import style from './styles/insum-menu.scss';

/**
 * Interface for input date type
 */
export interface SideMenuItem {
  label: string;
  parent?: string;
  available?: boolean;
  category?: boolean;
  hide?: boolean;
}

/**
 * Side menu web component
 */
@Define('insum-sidemenu')
export class SideMenu extends AbstractElement {
  /** list of hided menu items (after filter) */
  private _hideMenuItems: string[] = [];

  /** placeholder text for search input */
  @attr('search-placeholder')
  searchPlaceholder = '';

  /** menu data */
  @state()
  data: { [x: string]: SideMenuItem };
  /** last selected menu item */
  @state()
  activeMenuItem: string;

  constructor() {
    super(litRender, true);
  }

  /**
   * LIFECYCLE
   */
  connectedCallback(): void {
    super.connectedCallback();

    (this.shadowRoot.querySelector('[ref=insum-menu__sidenav_input]') as HTMLElement).focus();
  }

  /**
   * Create a sub-menu template base on data
   *
   * @param parent - parent item key
   */
  makeMenu(parent: string = null): TemplateResult[] {
    // nested menu level
    let deepLevel = 0;
    const findLevel = (item: SideMenuItem): void => {
      if (item !== undefined) {
        deepLevel++;
        if (item.parent !== undefined) {
          findLevel(this.data[item.parent]);
        }
      }
    };
    this.data && parent && findLevel(this.data[parent]);

    return (
      this.data &&
      Object.keys(this.data)
        .filter(key => (this.data[key].parent || null) === parent)
        .map(key => {
          const styles = [];
          this.data[key].available && styles.push('cursor: pointer');

          if (this.data[key].category) {
            const categoryCssClass = ['insum-menu-category__title'];
            this.activeMenuItem === key && categoryCssClass.push('insum-menu-category__title_active');
            return this.data[key].hide
              ? undefined
              : html`
                  <div class="insum-menu-category">
                    <span @click=${e => this.handleMenuLinkClick(e, key)} class=${categoryCssClass.join(' ')} style=${styles.join(';')}>
                      ${this.data[key].label}
                    </span>
                    ${this.makeMenu(key)}
                  </div>
                `;
          } else {
            const linkCssClass = ['insum-menu-category__link'];
            this.activeMenuItem === key && linkCssClass.push('insum-menu-category__link_active');
            this._hideMenuItems.includes(key) && linkCssClass.push('insum-menu-category__link_hide');

            styles.push(`padding-left: ${deepLevel * 10 + 10}px`);
            return this.data[key].hide
              ? undefined
              : html`
                  <div @click=${e => this.handleMenuLinkClick(e, key)} class=${linkCssClass.join(' ')} style=${styles.join(';')}>
                    ${this.data[key].label}
                  </div>
                  ${this.makeMenu(key)}
                `;
          }
        })
    );
  }

  /**
   * RENDER - service function
   */
  render(): TemplateResult {
    return html`
      <style>
        ${style}
      </style>
      <div class="insum-menu__sidenav">
        <div class="insum-menu-search">
          <insum-search-icon class="insum-menu-search__icon"></insum-search-icon>
          <input
            @keyup=${this.handleSearchKeyup.bind(this)}
            type="text"
            placeholder=${this.searchPlaceholder}
            ref="insum-menu__sidenav_input"
          />
        </div>
        ${this.makeMenu()}
        <slot></slot>
      </div>
    `;
  }

  /**
   * Handler for click by menu item
   *
   * @param event - click event
   * @param itemKey - item ID
   */
  handleMenuLinkClick(event: MouseEvent, itemKey: string): void {
    event.preventDefault();
    event.stopPropagation();
    if (this.data[itemKey].available) {
      this.activeMenuItem = itemKey;
      const changeActiveEvent = new CustomEvent('changeActive', {
        detail: {
          ...this.data[itemKey],
          key: itemKey,
        },
      });
      this.dispatchEvent(changeActiveEvent);
    }
  }

  /**
   * Handler for keyup inside search field
   *
   * @param event - keyup event
   */
  handleSearchKeyup(event: KeyboardEvent): void {
    event.preventDefault();
    if (event && event.target) {
      this.search(event.target['value']);
    }
  }

  /**
   * Filter menu items by text inside search field
   *
   * @param searchFor - text from search field
   */
  search(searchFor: string): void {
    this._hideMenuItems.length = 0;

    for (const item in this.data) {
      if (this.data[item].label.toLowerCase().indexOf(searchFor.toLowerCase()) === -1) {
        this._hideMenuItems.push(item);
      }
    }
    this.forceUpdate();
  }
}
