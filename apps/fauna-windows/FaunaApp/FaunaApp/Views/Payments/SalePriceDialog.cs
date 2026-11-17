using System;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Controls;
using FaunaApp.Core.Services;
using FaunaApp.UiIds;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The sold post's <i>Change price…</i> dialog (<c>feed-post-sale-price-dialog</c>;
/// monetization.md § The asking price → <i>Re-pricing a sold post</i>). A dumb painter:
/// the pre-fill is the shared <c>sale_price_prefill</c>, the save is ONE shared call
/// (<c>sale_price_save</c>) answering Unchanged / Saved (the dialog closes) or Refused
/// (the key's string lands on <c>error-message</c> and the dialog stays open with what
/// the author typed). The note on an advertised post is read from the catalog at run
/// time and painted only when the post is advertised.
///
/// <para>Lives under <c>Views\Payments\</c> — the whole directory is one removable
/// build item in the store-safe flavor (FaunaApp.csproj), so nothing here ships there.</para>
/// </summary>
internal static class SalePriceDialog
{
    /// <summary>Open the dialog for <paramref name="tierName"/> (the post's selling tier).
    /// Returns <c>true</c> once a save landed (the caller re-reads the feed), else
    /// <c>false</c> (cancelled, refused open, or the tier could not be read —
    /// the last reported through <paramref name="onError"/>).</summary>
    internal static async Task<bool> OpenAsync(
        XamlRoot xamlRoot, INestRpcClient rpc, string tierName, bool advertised, Action<string> onError)
    {
        FfiTierItem tier;
        try
        {
            var tiers = await rpc.SubscriptionTiersListAsync();
            tier = tiers.FirstOrDefault(t => t.name == tierName)
                ?? throw new InvalidOperationException($"no tier named {tierName}");
        }
        catch (Exception ex)
        {
            onError(S.Format("sale_price/error_load", ex.Message));
            return false;
        }

        var prefill = FaunaFfiMethods.SalePricePrefill(tier);
        var saved = false;

        var dialog = new ContentDialog { XamlRoot = xamlRoot, Title = S.Get("sale_price/title") };
        AutomationProperties.SetAutomationId(dialog, Ids.FeedPostSalePriceDialog);

        var hint = new TextBox
        {
            Text = prefill.priceHint,
            PlaceholderText = S.Get("feed/post/sell_price_placeholder"),
        };
        AutomationProperties.SetAutomationId(hint, Ids.FeedPostSalePriceHint);
        AutomationProperties.SetName(hint, S.Get("feed/post/sell_price_placeholder"));
        var asking = new TextBox
        {
            Text = prefill.askingPrice,
            PlaceholderText = S.Get("feed/post/sell_asking_price_placeholder"),
        };
        AutomationProperties.SetAutomationId(asking, Ids.FeedPostSalePriceAsking);
        AutomationProperties.SetName(asking, S.Get("feed/post/sell_asking_price_placeholder"));

        var save = new Button { Content = S.Get("common/save") };
        AutomationProperties.SetAutomationId(save, Ids.FeedPostSalePriceSaveButton);
        AutomationProperties.SetName(save, S.Get("common/save"));
        var cancel = new Button { Content = S.Get("common/cancel") };
        AutomationProperties.SetAutomationId(cancel, Ids.FeedPostSalePriceCancelButton);
        AutomationProperties.SetName(cancel, S.Get("common/cancel"));

        cancel.Click += (_, _) => dialog.Hide();
        save.Click += async (_, _) =>
        {
            save.IsEnabled = false;
            try
            {
                var answer = await rpc.SubscriptionSalePriceSaveAsync(tier, hint.Text, asking.Text);
                switch (answer)
                {
                    case FfiSalePriceSave.Refused refused:
                        onError(S.Get(refused.i18nKey.Replace('.', '/')));
                        break;
                    case FfiSalePriceSave.Saved:
                        saved = true;
                        dialog.Hide();
                        break;
                    default: // Unchanged: nothing was sent.
                        dialog.Hide();
                        break;
                }
            }
            catch (Exception ex)
            {
                onError(S.Format("sale_price/error_save", ex.Message));
            }
            finally
            {
                save.IsEnabled = true;
            }
        };

        var panel = new StackPanel { Spacing = 8, MinWidth = 360 };
        panel.Children.Add(hint);
        panel.Children.Add(asking);
        // Said only on an advertised post; the text is a public text read at run time.
        if (advertised)
        {
            var note = new TextBlock { Text = S.Get("sale_price/advertised_note"), TextWrapping = TextWrapping.Wrap, Opacity = 0.7 };
            AutomationProperties.SetAutomationId(note, Ids.FeedPostSalePriceAdvertisedNote);
            panel.Children.Add(note);
        }
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        actions.Children.Add(save);
        actions.Children.Add(cancel);
        panel.Children.Add(actions);
        dialog.Content = panel;

        await Dialogs.ShowAsync(dialog);
        return saved;
    }
}
