import { Define, AbstractElement, state } from 'abstract-element';
import litRender from 'abstract-element/render/lit';
import { html } from 'lit-html';
import style from './styles/insum-menu.scss';

/**
 * Interface for input date type
 */
interface SideMenuItem {
  title: string;
  hide: boolean;
  links: {
    id: string;
    title: string;
  }[];
}

/**
 * Web components
 */
@Define('insum-sidemenu')
export class SideMenu extends AbstractElement {
  /** list of hided menu items (after filter) */
  private _hideMenuItems: string[] = [];

  /** menu data */
  @state()
  data: SideMenuItem[] = [];
  /** last selected menu item */
  @state()
  activeMenuItem: string;

  constructor() {
    super(litRender, false);
  }

  /**
   * LIFECYCLE
   */
  connectedCallback(): void {
    super.connectedCallback();

    (this.querySelectorAll('.insum-menu__sidenav .insum-menu-search input')[0] as HTMLElement).focus();
  }

  /**
   * RENDER - service function
   */
  render() {
    const demoMenuCategories = this.data.map(menuItem => {
      let itemCountInside = 0;
      const linkElement = menuItem.links.map(link => {
        let cssClass = 'insum-menu-category__link';
        if (this._hideMenuItems.includes(link.id)) {
          cssClass += ' insum-menu-category__link_hide';
        } else {
          itemCountInside++;
        }

        cssClass = this.activeMenuItem === link.id ? `${cssClass} insum-menu-category__link_active` : cssClass;

        return html`
          <div class="${cssClass}" @click="${e => this.handleMenuLinkClick(e, link.id)}">${link.title}</div>
        `;
      });

      const hideClass = itemCountInside === 0 ? ' insum-menu-category_hide' : '';

      return html`
        <div class="${'insum-menu-category' + hideClass}">
          <label class="insum-menu-category__title" for="catid_${menuItem.title}">${menuItem.title}</label>
          <input type="checkbox" id="catid_${menuItem.title}" /> ${linkElement}
        </div>
      `;
    });

    return html`
      <style>
        ${style}
      </style>
      <div class="insum-menu__sidenav">
        <div class="insum-menu-search">
          <i class="material-icons">search</i>
          <input @keyup="${this.handleSearchKeyup.bind(this)}" type="text" placeholder="Поиск" />
        </div>
        ${demoMenuCategories}
      </div>
    `;
  }

  /**
   * Handler for click by menu item
   * @param event - click event
   * @param menuItemId - item title
   */
  handleMenuLinkClick(event: MouseEvent, menuItemId: string) {
    event.preventDefault();
    this.activeMenuItem = menuItemId;
    const click = new Event('change-menu-item');
    this.dispatchEvent(click);
    // document.querySelector('#content').textContent = menuItemTitle;
  }

  /**
   * Handler for keyup inside search field
   * @param event - keyup event
   */
  handleSearchKeyup(event: KeyboardEvent) {
    event.preventDefault();
    if (event && event.target) {
      this.search(event.target['value']);
    }
  }

  /**
   * Filter menu items by text inside search field
   * @param searchfor - text from search field
   */
  search(searchfor: string) {
    this._hideMenuItems.length = 0;

    for (const category of this.data) {
      for (const link of category.links) {
        if (link.title.toLowerCase().indexOf(searchfor.toLowerCase()) === -1) {
          this._hideMenuItems.push(link.id);
        }
      }
    }
    this.forceUpdate();
  }
}
