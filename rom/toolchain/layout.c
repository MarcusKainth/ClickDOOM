/* The engine's struct layout for RV32 ILP32, computed by the pinned toolchain.
 *
 * Not linked into the ROM. `make -C rom layout` compiles this with -S under
 * the same flags the ROM is built with, and the `@@` lines in the assembly
 * become build/layout.tsv. The compiler is the only thing that knows where
 * RV32 ILP32 puts a field, and the ROM carries no DWARF for a reader to ask
 * instead.
 *
 * Each LAYOUT_FIELD emits `struct field offset size`. LAYOUT_SIZE emits the
 * struct's own size under the field name `sizeof`, which is a keyword and so
 * cannot collide with a real field.
 *
 * An array field's size is its whole extent, so a reader divides to get the
 * element count and needs no separate table of the engine's dimension macros.
 */

#include <stddef.h>

/* The engine's headers do not include what they use, so this order is the
 * order they have to be read in. One block each, because clang-format sorts
 * within a block and sorting these does not compile. */
#include "doomtype.h"

#include "doomdef.h"

#include "doomdata.h"

#include "d_think.h"

#include "d_ticcmd.h"

#include "info.h"

#include "r_defs.h"

#include "d_player.h"

#include "p_mobj.h"

#include "p_pspr.h"

#include "p_spec.h"

#include "hu_lib.h"

/* "i" forces a compile-time constant, so a field name that does not exist is
 * a compile error rather than a missing row. */
#define LAYOUT_FIELD(type, field)                                              \
  __asm__ volatile("@@\t" #type "\t" #field                                    \
                   "\t%0\t%1" ::"i"(offsetof(type, field)),                    \
                   "i"(sizeof(((type *)0)->field)))

#define LAYOUT_SIZE(type)                                                      \
  __asm__ volatile("@@\t" #type "\tsizeof\t0\t%0" ::"i"(sizeof(type)))

/* Not static: the emitter has no caller, and a static function with none is
 * dropped before its body reaches the assembly. */
void clickdoom_layout(void) {
  LAYOUT_SIZE(thinker_t);
  LAYOUT_FIELD(thinker_t, prev);
  LAYOUT_FIELD(thinker_t, next);
  LAYOUT_FIELD(thinker_t, function);

  LAYOUT_SIZE(mapthing_t);
  LAYOUT_FIELD(mapthing_t, x);
  LAYOUT_FIELD(mapthing_t, y);
  LAYOUT_FIELD(mapthing_t, angle);
  LAYOUT_FIELD(mapthing_t, type);
  LAYOUT_FIELD(mapthing_t, options);

  LAYOUT_SIZE(mobj_t);
  LAYOUT_FIELD(mobj_t, thinker);
  LAYOUT_FIELD(mobj_t, x);
  LAYOUT_FIELD(mobj_t, y);
  LAYOUT_FIELD(mobj_t, z);
  LAYOUT_FIELD(mobj_t, angle);
  LAYOUT_FIELD(mobj_t, sprite);
  LAYOUT_FIELD(mobj_t, frame);
  LAYOUT_FIELD(mobj_t, subsector);
  LAYOUT_FIELD(mobj_t, floorz);
  LAYOUT_FIELD(mobj_t, ceilingz);
  LAYOUT_FIELD(mobj_t, radius);
  LAYOUT_FIELD(mobj_t, height);
  LAYOUT_FIELD(mobj_t, momx);
  LAYOUT_FIELD(mobj_t, momy);
  LAYOUT_FIELD(mobj_t, momz);
  LAYOUT_FIELD(mobj_t, type);
  LAYOUT_FIELD(mobj_t, tics);
  LAYOUT_FIELD(mobj_t, state);
  LAYOUT_FIELD(mobj_t, flags);
  LAYOUT_FIELD(mobj_t, health);
  LAYOUT_FIELD(mobj_t, movedir);
  LAYOUT_FIELD(mobj_t, movecount);
  LAYOUT_FIELD(mobj_t, target);
  LAYOUT_FIELD(mobj_t, reactiontime);
  LAYOUT_FIELD(mobj_t, threshold);
  LAYOUT_FIELD(mobj_t, player);
  LAYOUT_FIELD(mobj_t, lastlook);
  LAYOUT_FIELD(mobj_t, spawnpoint);
  LAYOUT_FIELD(mobj_t, tracer);

  LAYOUT_SIZE(ticcmd_t);
  LAYOUT_FIELD(ticcmd_t, forwardmove);
  LAYOUT_FIELD(ticcmd_t, sidemove);
  LAYOUT_FIELD(ticcmd_t, angleturn);
  LAYOUT_FIELD(ticcmd_t, buttons);

  LAYOUT_SIZE(pspdef_t);
  LAYOUT_FIELD(pspdef_t, state);
  LAYOUT_FIELD(pspdef_t, tics);
  LAYOUT_FIELD(pspdef_t, sx);
  LAYOUT_FIELD(pspdef_t, sy);

  LAYOUT_SIZE(player_t);
  LAYOUT_FIELD(player_t, mo);
  LAYOUT_FIELD(player_t, playerstate);
  LAYOUT_FIELD(player_t, cmd);
  LAYOUT_FIELD(player_t, viewz);
  LAYOUT_FIELD(player_t, viewheight);
  LAYOUT_FIELD(player_t, deltaviewheight);
  LAYOUT_FIELD(player_t, bob);
  LAYOUT_FIELD(player_t, health);
  LAYOUT_FIELD(player_t, armorpoints);
  LAYOUT_FIELD(player_t, armortype);
  LAYOUT_FIELD(player_t, powers);
  LAYOUT_FIELD(player_t, cards);
  LAYOUT_FIELD(player_t, backpack);
  LAYOUT_FIELD(player_t, readyweapon);
  LAYOUT_FIELD(player_t, pendingweapon);
  LAYOUT_FIELD(player_t, weaponowned);
  LAYOUT_FIELD(player_t, ammo);
  LAYOUT_FIELD(player_t, maxammo);
  LAYOUT_FIELD(player_t, attackdown);
  LAYOUT_FIELD(player_t, usedown);
  LAYOUT_FIELD(player_t, cheats);
  LAYOUT_FIELD(player_t, refire);
  LAYOUT_FIELD(player_t, killcount);
  LAYOUT_FIELD(player_t, itemcount);
  LAYOUT_FIELD(player_t, secretcount);
  LAYOUT_FIELD(player_t, message);
  LAYOUT_FIELD(player_t, damagecount);
  LAYOUT_FIELD(player_t, bonuscount);
  LAYOUT_FIELD(player_t, attacker);
  LAYOUT_FIELD(player_t, extralight);
  LAYOUT_FIELD(player_t, fixedcolormap);
  LAYOUT_FIELD(player_t, psprites);

  LAYOUT_SIZE(sector_t);
  LAYOUT_FIELD(sector_t, floorheight);
  LAYOUT_FIELD(sector_t, ceilingheight);
  LAYOUT_FIELD(sector_t, floorpic);
  LAYOUT_FIELD(sector_t, ceilingpic);
  LAYOUT_FIELD(sector_t, lightlevel);
  LAYOUT_FIELD(sector_t, special);
  LAYOUT_FIELD(sector_t, tag);
  LAYOUT_FIELD(sector_t, soundtraversed);
  LAYOUT_FIELD(sector_t, soundtarget);
  LAYOUT_FIELD(sector_t, specialdata);

  LAYOUT_SIZE(line_t);
  LAYOUT_FIELD(line_t, flags);
  LAYOUT_FIELD(line_t, special);
  LAYOUT_FIELD(line_t, tag);
  LAYOUT_FIELD(line_t, sidenum);

  LAYOUT_SIZE(side_t);
  LAYOUT_FIELD(side_t, textureoffset);
  LAYOUT_FIELD(side_t, rowoffset);
  LAYOUT_FIELD(side_t, toptexture);
  LAYOUT_FIELD(side_t, bottomtexture);
  LAYOUT_FIELD(side_t, midtexture);

  LAYOUT_SIZE(subsector_t);
  LAYOUT_FIELD(subsector_t, sector);
  LAYOUT_FIELD(subsector_t, numlines);
  LAYOUT_FIELD(subsector_t, firstline);

  LAYOUT_SIZE(vldoor_t);
  LAYOUT_FIELD(vldoor_t, thinker);
  LAYOUT_FIELD(vldoor_t, type);
  LAYOUT_FIELD(vldoor_t, sector);
  LAYOUT_FIELD(vldoor_t, topheight);
  LAYOUT_FIELD(vldoor_t, speed);
  LAYOUT_FIELD(vldoor_t, direction);
  LAYOUT_FIELD(vldoor_t, topwait);
  LAYOUT_FIELD(vldoor_t, topcountdown);

  LAYOUT_SIZE(plat_t);
  LAYOUT_FIELD(plat_t, thinker);
  LAYOUT_FIELD(plat_t, sector);
  LAYOUT_FIELD(plat_t, speed);
  LAYOUT_FIELD(plat_t, low);
  LAYOUT_FIELD(plat_t, high);
  LAYOUT_FIELD(plat_t, wait);
  LAYOUT_FIELD(plat_t, count);
  LAYOUT_FIELD(plat_t, status);
  LAYOUT_FIELD(plat_t, oldstatus);
  LAYOUT_FIELD(plat_t, crush);
  LAYOUT_FIELD(plat_t, tag);
  LAYOUT_FIELD(plat_t, type);

  LAYOUT_SIZE(floormove_t);
  LAYOUT_FIELD(floormove_t, thinker);
  LAYOUT_FIELD(floormove_t, type);
  LAYOUT_FIELD(floormove_t, crush);
  LAYOUT_FIELD(floormove_t, sector);
  LAYOUT_FIELD(floormove_t, direction);
  LAYOUT_FIELD(floormove_t, newspecial);
  LAYOUT_FIELD(floormove_t, texture);
  LAYOUT_FIELD(floormove_t, floordestheight);
  LAYOUT_FIELD(floormove_t, speed);

  LAYOUT_SIZE(ceiling_t);
  LAYOUT_FIELD(ceiling_t, thinker);
  LAYOUT_FIELD(ceiling_t, type);
  LAYOUT_FIELD(ceiling_t, sector);
  LAYOUT_FIELD(ceiling_t, bottomheight);
  LAYOUT_FIELD(ceiling_t, topheight);
  LAYOUT_FIELD(ceiling_t, speed);
  LAYOUT_FIELD(ceiling_t, crush);
  LAYOUT_FIELD(ceiling_t, direction);
  LAYOUT_FIELD(ceiling_t, tag);
  LAYOUT_FIELD(ceiling_t, olddirection);

  LAYOUT_SIZE(lightflash_t);
  LAYOUT_FIELD(lightflash_t, thinker);
  LAYOUT_FIELD(lightflash_t, sector);
  LAYOUT_FIELD(lightflash_t, count);
  LAYOUT_FIELD(lightflash_t, maxlight);
  LAYOUT_FIELD(lightflash_t, minlight);
  LAYOUT_FIELD(lightflash_t, maxtime);
  LAYOUT_FIELD(lightflash_t, mintime);

  LAYOUT_SIZE(strobe_t);
  LAYOUT_FIELD(strobe_t, thinker);
  LAYOUT_FIELD(strobe_t, sector);
  LAYOUT_FIELD(strobe_t, count);
  LAYOUT_FIELD(strobe_t, minlight);
  LAYOUT_FIELD(strobe_t, maxlight);
  LAYOUT_FIELD(strobe_t, darktime);
  LAYOUT_FIELD(strobe_t, brighttime);

  LAYOUT_SIZE(glow_t);
  LAYOUT_FIELD(glow_t, thinker);
  LAYOUT_FIELD(glow_t, sector);
  LAYOUT_FIELD(glow_t, minlight);
  LAYOUT_FIELD(glow_t, maxlight);
  LAYOUT_FIELD(glow_t, direction);

  LAYOUT_SIZE(fireflicker_t);
  LAYOUT_FIELD(fireflicker_t, thinker);
  LAYOUT_FIELD(fireflicker_t, sector);
  LAYOUT_FIELD(fireflicker_t, count);
  LAYOUT_FIELD(fireflicker_t, maxlight);
  LAYOUT_FIELD(fireflicker_t, minlight);

  LAYOUT_SIZE(button_t);
  LAYOUT_FIELD(button_t, line);
  LAYOUT_FIELD(button_t, where);
  LAYOUT_FIELD(button_t, btexture);
  LAYOUT_FIELD(button_t, btimer);

  LAYOUT_SIZE(state_t);
  LAYOUT_FIELD(state_t, sprite);
  LAYOUT_FIELD(state_t, frame);
  LAYOUT_FIELD(state_t, tics);
  LAYOUT_FIELD(state_t, action);
  LAYOUT_FIELD(state_t, nextstate);
  LAYOUT_FIELD(state_t, misc1);
  LAYOUT_FIELD(state_t, misc2);

  LAYOUT_SIZE(hu_textline_t);
  LAYOUT_FIELD(hu_textline_t, l);
  LAYOUT_FIELD(hu_textline_t, len);

  LAYOUT_SIZE(hu_stext_t);
  LAYOUT_FIELD(hu_stext_t, l);
  LAYOUT_FIELD(hu_stext_t, h);
  LAYOUT_FIELD(hu_stext_t, cl);

  /* Only the size: a mobj's info pointer becomes an index into the
   * mobjinfo array, and nothing reads the entry itself. */
  LAYOUT_SIZE(mobjinfo_t);
}
