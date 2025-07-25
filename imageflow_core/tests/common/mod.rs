
extern crate libc;
extern crate imageflow_types as s;
extern crate imageflow_helpers as hlp;
extern crate serde_json;
extern crate dssim;
extern crate rgb;

extern crate itertools;
extern crate twox_hash;
extern crate imgref;
use std::marker::PhantomPinned;
use std::ffi::CString;
use std::path::Path;
use imageflow_core::graphics::bitmaps::BitmapWindowMut;
use imageflow_core::{Context, FlowError, ErrorKind};

use s::PixelLayout;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::io::Write;
use std;
use std::pin::Pin;
use imageflow_core;

use std::sync::RwLock;
use imageflow_types::{ Node, ResponsePayload};
use std::time::Duration;
use imageflow_core::BitmapKey;
use slotmap::Key;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ChecksumMatch {
    Match,
    Mismatch,
    NewStored,
}

#[derive(Clone, Debug, PartialEq)]
pub enum IoTestEnum {
    // #[serde(rename="bytes_hex")]
    // BytesHex(String),
    // #[serde(rename="base_64")]
    // Base64(String),
    ByteArray(Vec<u8>),
    // #[serde(rename="file")]
    // Filename(String),
    OutputBuffer,
    // #[serde(rename="output_base_64")]
    // OutputBase64,
    // /// To be replaced before execution
    // #[serde(rename="placeholder")]
    //Placeholder,
    Url(String)
}


pub fn get_url_bytes_with_retry(url: &str) -> Result<Vec<u8>, FlowError> {
    let mut retry_count = 3;
    let mut retry_wait = 100;
    loop {
        match ::imageflow_http_helpers::fetch_bytes(&url)
            .map_err(|e| nerror!(ErrorKind::FetchError, "{}: {}", url, e)){
            Err(e) => {
                if retry_count > 0{
                    retry_count -= 1;
                    std::thread::sleep(Duration::from_millis( retry_wait));
                    retry_wait *= 5;
                }else{
                    return Err(e)
                }
            }
            Ok(bytes) => {
                return Ok(bytes);
            }
        }
    }
}

pub struct IoTestTranslator;
impl IoTestTranslator {
    pub fn add(&self,c: &mut Context,
           io_id: i32,
           io_enum: IoTestEnum)
           -> Result<(), FlowError> {
        match io_enum {
            IoTestEnum::ByteArray(vec) => {
                c.add_copied_input_buffer(io_id, &vec).map_err(|e| e.at(here!()))
            }
            // IoTestEnum::Base64(b64_string) => {
            //     //TODO: test and disable slow methods
            //     let bytes = b64_string.as_str().from_base64()
            //         .map_err(|e| nerror!(ErrorKind::InvalidArgument, "base64: {}", e))?;
            //     c.add_copied_input_buffer(io_id, &bytes).map_err(|e| e.at(here!()))
            // }
            // IoTestEnum::BytesHex(hex_string) => {
            //     let bytes = hex_string.as_str().from_hex().unwrap();
            //     c.add_copied_input_buffer(io_id, &bytes).map_err(|e| e.at(here!()))
            // }
            // IoTestEnum::Filename(path) => {
            //
            //     c.add_file(io_id, dir, &path )
            // }
            IoTestEnum::Url(url) => {
                let bytes = get_url_bytes_with_retry(&url).map_err(|e| e.at(here!()))?;
                c.add_input_vector(io_id, bytes).map_err(|e| e.at(here!()))
            },

            IoTestEnum::OutputBuffer  => {
                c.add_output_buffer(io_id).map_err(|e| e.at(here!()))
            },
            // IoTestEnum::Placeholder => {
            //     Err(nerror!(ErrorKind::GraphInvalid, "Io Placeholder {} was never substituted", io_id))
            // }
        }
    }


}




pub fn build_steps(context: &mut Context, steps: &[s::Node], io: Vec<IoTestEnum>, security: Option<imageflow_types::ExecutionSecurity>,  debug: bool) -> Result<ResponsePayload, FlowError>{

    build_framewise(context, s::Framewise::Steps(steps.to_vec()), io, security, debug).map_err(|e| e.at(here!()))
}



pub fn build_framewise(context: &mut Context, framewise: s::Framewise, io: Vec<IoTestEnum>, security: Option<imageflow_types::ExecutionSecurity>,  debug: bool) -> Result<ResponsePayload, FlowError>{

    for (ix, val) in io.into_iter().enumerate() {
        IoTestTranslator{}.add(context, ix as i32, val)?;
    }
    let build = s::Execute001{
        security,
        graph_recording: default_graph_recording(debug),
        framewise: framewise
    };
    if debug {
        println!("{}", serde_json::to_string_pretty(&build).unwrap());
    }

    context.execute_1(build)
}

/// Executes the given steps (adding a frame buffer container to the end of them).
/// Returns the width and height of the resulting frame.
/// Steps must be open-ended - they cannot be terminated with an encoder.
pub fn get_result_dimensions(steps: &[s::Node], io: Vec<IoTestEnum>, debug: bool) -> (u32, u32) {
    let mut bit = BitmapBgraContainer::empty();
    let mut steps = steps.to_vec();
    steps.push(unsafe { bit.as_mut().get_node() });

    let mut context = Context::create().unwrap();

    let result = build_steps(&mut context, &steps, io, None, debug).unwrap();

    if let Some((w, h)) = bit.bitmap_size(&context) {
        (w as u32, h as u32)
    }else{
        panic!("execution failed: {:?}", result);
    }
}

/// Just validates that no errors are thrown during job execution
pub fn smoke_test(input: Option<IoTestEnum>, output: Option<IoTestEnum>, security: Option<imageflow_types::ExecutionSecurity>, debug: bool, steps: Vec<s::Node>) -> Result<s::ResponsePayload, imageflow_core::FlowError>{
    let mut io_list = Vec::new();
    if input.is_some() {
        io_list.push(input.unwrap());
    }
    if output.is_some() {
        io_list.push(output.unwrap());
    }
    let mut context = Context::create().unwrap();
    build_steps(&mut context, &steps, io_list, security, debug)
}


/// A context for getting/storing frames and frame checksums by test name.
/// Currently has read-only support for remote storage.
/// TODO: Add upload support; it's very annoying to do it manually
pub struct ChecksumCtx{
    checksum_file: PathBuf,
    url_list_file: PathBuf,
    visuals_dir: PathBuf,
    #[allow(dead_code)]
    cache_dir: PathBuf,
    create_if_missing: bool,
    url_base: &'static str
}

lazy_static! {
    static ref CHECKSUM_FILE: RwLock<()> = RwLock::new(());
}

impl ChecksumCtx{

    /// A checksum context configured for tests/visuals/*
    pub fn visuals() -> ChecksumCtx{
        let visuals = Path::new(env!("CARGO_MANIFEST_DIR")).join(Path::new("tests")).join(Path::new("visuals"));
        std::fs::create_dir_all(&visuals).unwrap();
        ChecksumCtx {
            visuals_dir: visuals.clone(),
            cache_dir: visuals.join(Path::new("cache")),
            create_if_missing: true,
            checksum_file: visuals.join(Path::new("checksums.json")),
            url_list_file:  visuals.join(Path::new("images.txt")),
            url_base: "https://s3-us-west-2.amazonaws.com/imageflow-resources/visual_test_checksums/"
        }
    }

    /// Load the checksum map
    fn load_list(&self) -> Result<BTreeMap<String,String>,()>{
        if self.checksum_file.exists() {
            let map: BTreeMap<String, String> = ::serde_json::from_reader(::std::fs::File::open(&self.checksum_file).unwrap()).unwrap();
            Ok(map)
        }else{
            Ok(BTreeMap::new())
        }
    }

    /// Save the checksum map and url_list to disk
    fn save_list(&self, map: &BTreeMap<String,String>) -> Result<(),()>{
        let mut f = ::std::fs::File::create(&self.checksum_file).unwrap();
        ::serde_json::to_writer_pretty(&mut f, map).unwrap();
        f.sync_all().unwrap();
        // Write the URL list
        // We can use this to prefetch required images in the background on CI)
        // TODO: add simple script to do this
        let mut f = ::std::fs::File::create(&self.url_list_file).unwrap();
        use self::itertools::Itertools;
        let list_contents = map.values().map(|key| self.image_url(key)).join("\n");
        f.write_all(list_contents.as_bytes()).unwrap();
        f.sync_all().unwrap();
        Ok(())
    }


    /// Get the stored result checksum for a named test
    #[allow(unused_variables)]
    pub fn get(&self, name: &str) -> Option<String>{
        #[allow(unused_variables)]
        let lock = CHECKSUM_FILE.read().unwrap();
        self.load_list().unwrap().get(name).and_then(|v|Some(v.to_owned()))
    }

    /// Set the result checksum for a named test
    /// Doesn't work right under nextest when new checksums are added
    #[allow(unused_variables)]
    pub fn set(&self, name: String, checksum: String) -> Result<(),()>{
        #[allow(unused_variables)]
        let lock = CHECKSUM_FILE.write().unwrap();
        let mut map = self.load_list().unwrap();
        map.insert(name,checksum);
        self.save_list(&map).unwrap();
        Ok(())
    }


    pub fn image_url(&self, checksum: &str) -> String{
        if !checksum.contains("."){
            format!("{}{}.png",self.url_base, checksum)
        }else{
            format!("{}{}", self.url_base, checksum)
        }
    }

    pub fn image_path(&self, checksum: &str) -> PathBuf{
        let name = if !checksum.contains("."){
            format!("{}.png", checksum)
        }else{
            format!("{}", checksum)
        };

        self.visuals_dir.as_path().join(Path::new(&name))
    }

    pub fn image_path_string(&self, checksum: &str) -> String{
        self.image_path(checksum).into_os_string().into_string().unwrap()
    }
    pub fn image_path_cstring(&self, checksum: &str) -> CString{
        CString::new(self.image_path_string(checksum)).unwrap()
    }
    /// Fetch the given image to disk
    pub fn fetch_image(&self, checksum: &str){
        let dest_path = self.image_path(checksum);
        let source_url = self.image_url(checksum);
        if dest_path.exists() {
            println!("{} (trusted) exists", checksum);
        }else{
            print!("Fetching {} to {:?}...", &source_url, &dest_path);
            let bytes = get_url_bytes_with_retry(&source_url).expect("Did you forget to upload {} to s3?");
            let mut f = File::create(&dest_path).unwrap();
            f.write_all(bytes.as_ref()).unwrap();
            f.flush().unwrap();
            f.sync_all().unwrap();


            println!("{} bytes written successfully.", bytes.len());
        }
    }

    /// Load the given image from disk (and download it if it's not on disk)
    /// The bitmap will be destroyed when the returned Context goes out of scope
    pub fn load_image(&self, checksum: &str) -> (Box<Context>, BitmapKey) {
        self.fetch_image(checksum);

        let mut c = Context::create().unwrap();
        let path = self.image_path_string(checksum);
        c.add_file(0, s::IoDirection::In, &path).unwrap();

        let image =  decode_image(&mut *c, 0);
        (c, image)
    }


    /// Save the given image to disk by calculating its checksum.
    pub fn save_frame(&self, window: &mut BitmapWindowMut<u8>, checksum: &str){
        let dest_path = self.image_path(&checksum);
        if !dest_path.exists(){
            let path_str = dest_path.to_str();
            if let Some(path) = path_str{
                println!("Writing {}", &path);
            }else {
                println!("Writing {:#?}", &dest_path);
            }
            imageflow_core::helpers::write_png(dest_path, window).unwrap();
        }
    }
    /// Save the given bytes to disk by calculating their checksum.
    pub fn save_bytes(&self, bytes: &[u8], checksum: &str){
        let dest_path = self.image_path(&checksum);
        if !dest_path.exists(){
            println!("Writing {:?}", &dest_path);
            let mut f = ::std::fs::File::create(&dest_path).unwrap();
            f.write_all(bytes).unwrap();
            f.sync_all().unwrap();
        }
    }

    /// We include the file extension in checksums of encoded images, as we can't be sure they're stored as PNG (as we can with frame checksums)
    pub fn checksum_bytes(bytes: &[u8]) -> String {
        format!("{:02$X}.{1}", hlp::hashing::hash_64(bytes), Self::file_extension_for_bytes(bytes), 17)
    }

    pub fn file_extension_for_bytes(bytes: &[u8]) -> &'static str{
        if bytes.starts_with(&[0x89,0x50,0x4E,0x47,0x0D,0x0A,0x1A,0x0A]){
            "png"
        }else if bytes.starts_with(b"GIF8"){
            "gif"
        }else if bytes.starts_with(&[0xFF,0xD8,0xFF]) {
            "jpg"
        } else if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && bytes[8..12].starts_with(b"WEBP"){
            "webp"
        } else{
            "unknown"
        }
    }

    /// Provides a checksum composed of two hashes - one from the pixels, one from the dimensions and format
    /// This format is preserved from legacy C tests, thus its rudimentary (but, I suppose, sufficient) nature.
    pub fn checksum_bitmap_window(bitmap_window: &mut BitmapWindowMut<u8>) -> String {
        let info = format!("{}x{} fmt={}", bitmap_window.w(), bitmap_window.h(), bitmap_window.info().calculate_pixel_format().unwrap() as i32);

        return format!("{:02$X}_{:02$X}", bitmap_window.short_hash_pixels(), hlp::hashing::legacy_djb2(info.as_bytes()), 17)

    }


    /// Checksums the result, saves it to disk, the compares the actual checksum to the expected checksum.
    ///
    /// Complains loudly and returns false if the checksums don't match. Also returns the trusted checksum.
    ///
    /// if there is no trusted checksum, create_if_missing is set, then
    /// the checksum will be stored, and the function will return true.
    pub fn bitmap_matches(&self, c: &Context, bitmap_key: BitmapKey, name: &str) -> (ChecksumMatch, String){


        let bitmaps = c.borrow_bitmaps()
            .map_err(|e| e.at(here!())).unwrap();

        let mut bitmap = bitmaps.try_borrow_mut(bitmap_key)
            .map_err(|e| e.at(here!())).unwrap();

        let mut window = bitmap.get_window_u8().unwrap();

        window.normalize_unused_alpha().unwrap();

        let actual = Self::checksum_bitmap_window(&mut window);
        //println!("actual = {}", &actual);
        // Always write a copy if it doesn't exist
        self.save_frame(&mut window, &actual);

        self.exact_match(actual, name)
    }

    /// Checksums the result, saves it to disk, the compares the actual checksum to the expected checksum.
    ///
    /// Complains loudly and returns false if the checksums don't match. Also returns the trusted checksum.
    ///
    /// if there is no trusted checksum, create_if_missing is set, then
    /// the checksum will be stored, and the function will return true.
    pub fn bytes_match(&self, bytes: &[u8], name: &str) -> (ChecksumMatch, String){
        let actual = Self::checksum_bytes(bytes);

        //println!("actual = {}", &actual);

        // Always write a copy if it doesn't exist
        self.save_bytes(bytes, &actual);

        self.exact_match(actual, name)
    }


    /// Compares the actual checksum to the expected checksum. Returns the trusted checksum.
    ///
    /// Complains loudly and returns false if the checksums don't match.
    ///
    /// if there is no trusted checksum, create_if_missing is set, then
    /// the checksum will be stored, and the function will return true.
    pub fn exact_match(&self, actual_checksum: String, name: &str) -> (ChecksumMatch, String){
        if let Some(trusted) = self.get(name){
            if trusted == actual_checksum{
                (ChecksumMatch::Match, trusted)
            }else{
                println!("====================\n{}\nThe stored checksum {} differs from the actual_checksum one {}\nTrusted: {}\nActual: {}\n",
                         name, &trusted,
                         &actual_checksum,
                         self.image_path(&trusted).to_str().unwrap(),
                         self.image_path(&actual_checksum).to_str().unwrap());
                (ChecksumMatch::Mismatch, trusted)
            }
        }else{
            if self.create_if_missing {
                println!("====================\n{}\nStoring checksum {}", name, &actual_checksum);
                self.set(name.to_owned(), actual_checksum.clone()).unwrap();
                (ChecksumMatch::NewStored, actual_checksum)
            } else {
                panic!("There is no stored checksum for {}; rerun with create_if_missing=true", name);
            }
        }
    }

    // TODO: implement uploader
}

pub fn decode_image(c: &mut Context, io_id: i32) -> BitmapKey {
    let mut bit = BitmapBgraContainer::empty();
    let result = c.execute_1(s::Execute001 {
        graph_recording: None,
        security: None,
        framewise: s::Framewise::Steps(vec![
            s::Node::Decode {
                io_id,
                commands: None
            },
            unsafe { bit.as_mut().get_node() }
        ])
    });

    result.unwrap();
    unsafe{ bit.bitmap_key(c).unwrap() }
}

pub fn decode_input(c: &mut Context, input: IoTestEnum) -> BitmapKey {
    let mut bit = BitmapBgraContainer::empty();

    let _result = build_steps(c, &vec![
        s::Node::Decode {
            io_id: 0,
            commands: None
        },
        unsafe { bit.as_mut().get_node() }
    ], vec![input], None, false).unwrap();

    unsafe { bit.bitmap_key(c).unwrap() }
}




/// Returns the number of bytes that differ, followed by the total value of all differences
/// If these are equal, then only off-by-one errors are occurring
// fn diff_bytes(a: &[u8], b: &[u8]) ->(i64,i64){
//     a.iter().zip(b.iter()).fold((0,0), |(count, delta), (a,b)| if a != b { (count + 1, delta + (i64::from(*a) - i64::from(*b)).abs()) } else { (count,delta)})
// }

fn diff_bytes(a: &[u8], b: &[u8]) -> (i64, i64, i64) {
    let mut count = 0;
    let mut premultiplied_delta = 0;
    let mut abs_diff = 0;

    for (a_pixel, b_pixel) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let a_alpha = a_pixel[3] as f32 / 255.0;
        let b_alpha = b_pixel[3] as f32 / 255.0;

        if a_pixel != b_pixel {
            count += 1;

            // Calculate premultiplied delta for RGB channels
            for i in 0..3 {
                let a_premultiplied = (a_pixel[i] as f32 * a_alpha).round() as i64;
                let b_premultiplied = (b_pixel[i] as f32 * b_alpha).round() as i64;
                premultiplied_delta += (a_premultiplied - b_premultiplied).abs();
            }

            // Add alpha channel difference to premultiplied delta
            premultiplied_delta += (i64::from(a_pixel[3]) - i64::from(b_pixel[3])).abs();

            // Calculate absolute difference for all channels
            for i in 0..4 {
                abs_diff += (i64::from(a_pixel[i]) - i64::from(b_pixel[i])).abs();
            }
        }
    }

    (count, premultiplied_delta, abs_diff)
}



fn diff_bitmap_windows(a: &mut BitmapWindowMut<u8>, b: &mut BitmapWindowMut<u8>) -> (i64, i64, i64) {
    if a.w() != b.w() || a.h() != b.h() || a.info().pixel_layout() != b.info().pixel_layout() {
        panic!("Bitmap dimensions differ. a:\n{:#?}\nb:\n{:#?}", a, b);
    }
    if a.info().pixel_layout() != PixelLayout::BGRA {
        panic!("Bitmap layout is not BGRA");
    }

    a.scanlines().into_iter().zip(b.scanlines().into_iter()).map(|(a_scanline, b_scanline)| {

        if a_scanline.row() == b_scanline.row() {
            (0, 0, 0)
        } else {
            diff_bytes(a_scanline.row(), b_scanline.row())
        }
    }).fold((0, 0, 0), |(a, b, c), (d, e, f)| (a + d, b + e, c + f))
}



#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Similarity{
    AllowOffByOneBytesCount(i64),
    AllowOffByOneBytesRatio(f32),
    AllowDssimMatch(f64, f64)
}

impl Similarity{
    fn report_on_bytes(&self, count: i64, premultiplied_delta: i64, abs_diff: i64, len: usize) -> Option<String>{
        let allowed_off_by_one_bytes: i64 = match *self {
            Similarity::AllowOffByOneBytesCount(v) => v,
            Similarity::AllowOffByOneBytesRatio(ratio) => (ratio * len as f32) as i64,
            Similarity::AllowDssimMatch(..) => return None,
        };
        eprintln!("{} {} {} {} {:?}", count, premultiplied_delta, abs_diff, len, self);

        //TODO: This doesn't really work, since off-by-one errors are averaged and thus can hide +/- 4

        if count < premultiplied_delta / 4 || premultiplied_delta > allowed_off_by_one_bytes{
            let premult_degree = premultiplied_delta as f64 / (count * 4) as f64;
            let abs_degree = abs_diff as f64 / (count * 4) as f64;
            return Some(format!("Bitmaps mismatched: after adjusting for transparency, an average channel error of {} (total {}) on {} ({}% of {}) pixels. Absolute error avg {} (total {})",
                                premult_degree, premultiplied_delta, count, (count as f64 * 100f64 / len as f64), len, abs_degree, abs_diff));
        }

        None
    }
}

#[derive(Clone)]
pub struct Constraints{
    pub similarity: Similarity,
    pub max_file_size: Option<usize>
}

pub enum ResultKind<'a>{
    Bitmap{ context: &'a Context, key: BitmapKey},
    Bytes(&'a [u8])
}
impl<'a> ResultKind<'a>{
    fn exact_match_verbose(&mut self, c: &ChecksumCtx, name: &str) -> (ChecksumMatch, String){
        match *self{
            ResultKind::Bitmap{ context, key} => c.bitmap_matches(context,key, name),
            ResultKind::Bytes(ref b) => c.bytes_match(b, name)
        }
    }
}

fn get_imgref_bgra32(b: &mut BitmapWindowMut<u8>) -> imgref::ImgVec<rgb::Rgba<f32>> {
    use self::dssim::*;

    b.normalize_unused_alpha().unwrap();
    if b.info().pixel_layout() != PixelLayout::BGRA{
        panic!("Pixel layout is not BGRA");
    }

    let (w, h) = (b.w() as usize, b.h() as usize);

    let slice = b.get_slice();
    let new_stride = b.info().t_stride() as usize / 4;

    let cast_to_bgra8 = bytemuck::cast_slice::<u8,rgb::alt::BGRA8>(slice);

    imgref::Img::new_stride(cast_to_bgra8.to_rgbaplu(), w, h, new_stride)
}

/// Compare two bgra32 or bgr32 frames using the given similarity requirements
pub fn compare_bitmaps(_c: &ChecksumCtx, mut actual: &mut BitmapWindowMut<u8>, mut expected: &mut BitmapWindowMut<u8>, require: Similarity, panic: bool) -> bool{
    let (count, premultiplied_delta, abs_diff) = diff_bitmap_windows(&mut actual, &mut expected);
    if count == 0 {
        return true;
    }
    if let Similarity::AllowDssimMatch(minval, maxval) = require {
        let actual_ref = get_imgref_bgra32(&mut actual);
        let expected_ref = get_imgref_bgra32(&mut expected);
        let d = dssim::new();

        let actual_img = d.create_image(&actual_ref).unwrap();
        let expected_img = d.create_image(&expected_ref).unwrap();

        let (dssim, _) = d.compare(&expected_img, actual_img);

        let failure = if dssim > maxval {
            Some(format!("The dssim {} is greater than the permitted value {}", dssim, maxval))
        } else if dssim < minval {
            Some(format!("The dssim {} is lower than expected minimum value {}", dssim, minval))
        } else {
            None
        };

        if let Some(message) = failure {
            if panic {
                panic!("{}", message);
            } else {
                eprintln!("{}", message);
                false
            }
        } else {
            true
        }
    } else {
        if let Some(message) = require.report_on_bytes(count, premultiplied_delta, abs_diff, actual.info().width() as usize * actual.info().height() as usize * actual.t_per_pixel() as usize) {
            if panic {
                panic!("{}", message);
            } else {
                eprintln!("{}", message);
                return false;
            }
        }
        true
    }

}

/// Evaluates the given result against known truth, applying the given constraints
pub fn compare_with<'a, 'b>(c: &ChecksumCtx, expected_checksum: &str, expected_context: &Context,  expected_bitmap_key: BitmapKey, result: ResultKind<'a>, require: Constraints, panic: bool) -> bool{
    if !check_size(&result, require.clone(), panic) {
        return false;
    }


    let mut image_context = Context::create().unwrap();
    let (actual_context, actual_bitmap_key) = match result {
        ResultKind::Bitmap{ context, key } => (context, key ),
        ResultKind::Bytes(actual_bytes) => {
            let key = decode_input(&mut image_context, IoTestEnum::ByteArray(actual_bytes.to_vec()));
            (image_context.as_ref(), key)
        }
    };



    let actual_bitmaps = actual_context.borrow_bitmaps()
        .map_err(|e| e.at(here!())).unwrap();

    let mut actual_bitmap = actual_bitmaps.try_borrow_mut(actual_bitmap_key)
        .map_err(|e| e.at(here!())).unwrap();

    let mut actual = actual_bitmap.get_window_u8().unwrap();

    let result_checksum =  ChecksumCtx::checksum_bitmap_window(&mut actual);

    if result_checksum == expected_checksum {
        true
    } else{
        let expected_bitmaps = expected_context.borrow_bitmaps()
            .map_err(|e| e.at(here!())).unwrap();

        let mut expected_bitmap = expected_bitmaps.try_borrow_mut(expected_bitmap_key)
            .map_err(|e| e.at(here!())).unwrap();
        let mut expected = expected_bitmap.get_window_u8().unwrap();

        compare_bitmaps(c, &mut actual, &mut expected, require.similarity, panic)
    }
}

pub fn check_size(result: &ResultKind, require: Constraints, panic: bool) -> bool{
    if let ResultKind::Bytes(ref actual_bytes) = *result {
        if actual_bytes.len() > require.max_file_size.unwrap_or(actual_bytes.len()) {
            let message = format!("Encoded size ({}) exceeds limit ({})", actual_bytes.len(), require.max_file_size.unwrap());
            if panic {
                panic!("{}", &message);
            } else {
                eprintln!("{}", &message);
                return false;
            }
        }

    }
    true
}




/// Evaluates the given result against known truth, applying the given constraints
pub fn evaluate_result<'a>(c: &ChecksumCtx, name: &str, mut result: ResultKind<'a>, require: Constraints, panic: bool) -> bool{
    let (exact, trusted) = result.exact_match_verbose(c, name);


    if !check_size(&result, require.clone(), panic) {
        return false;
    }


    if exact == ChecksumMatch::Match {
        true
    } else {
        let (expected_context, expected_bitmap_key) = c.load_image(&trusted);
        let mut image_context = Context::create().unwrap();
        let (actual_context, actual_bitmap_key) = match result {
            ResultKind::Bitmap{ context, key } => (context, key),
            ResultKind::Bytes(actual_bytes) => {
                image_context.add_input_bytes(0, actual_bytes).unwrap();
                let key =  decode_image(&mut image_context, 0);
                (image_context.as_ref(),key )
            }
        };
        let res ;
        {
                let expected_bitmaps = expected_context.borrow_bitmaps()
                .map_err(|e| e.at(here!())).unwrap();

            let mut expected_bitmap = expected_bitmaps.try_borrow_mut(expected_bitmap_key)
                .map_err(|e| e.at(here!())).unwrap();
            let mut expected = expected_bitmap.get_window_u8().unwrap();

            let actual_bitmaps = actual_context.borrow_bitmaps()
                .map_err(|e| e.at(here!())).unwrap();

            let mut actual_bitmap = actual_bitmaps.try_borrow_mut(actual_bitmap_key)
                .map_err(|e| e.at(here!())).unwrap();
            let mut actual = actual_bitmap.get_window_u8().unwrap();

            res = compare_bitmaps(c, &mut actual, &mut expected, require.similarity, panic);
        }
        drop(expected_context); // Context must remain in scope until we are done with expected_bitmap
        res
    }
}

/// Complains loudly and returns false  if `bitmap` doesn't match the stored checksum and isn't within the off-by-one grace window.
pub fn bitmap_regression_check(c: &ChecksumCtx, context: &Context, bitmap_key: BitmapKey, name: &str, allowed_off_by_one_bytes: usize) -> bool{

    evaluate_result(c,  name, ResultKind::Bitmap{context, key: bitmap_key}, Constraints{
        similarity: Similarity::AllowOffByOneBytesCount(allowed_off_by_one_bytes as i64),
        max_file_size: None
    }, true)
}




/// Compares the bitmap frame result of a given job to the known good checksum. If there is a checksum mismatch, a percentage of off-by-one bytes can be allowed.
/// If no good checksum has been stored, pass 'store_if_missing' in order to add it.
/// If you accidentally store a bad checksum, just delete it from the JSON file manually.
///
pub fn compare(input: Option<IoTestEnum>, allowed_off_by_one_bytes: usize, checksum_name: &str, store_if_missing: bool, debug: bool, steps: Vec<s::Node>) -> bool {

    compare_multiple(input.map(|i| vec![i]), allowed_off_by_one_bytes, checksum_name, store_if_missing, debug, steps)
}
pub fn compare_multiple(inputs: Option<Vec<IoTestEnum>>, allowed_off_by_one_bytes: usize, checksum_name: &str, store_if_missing: bool, debug: bool, steps: Vec<s::Node>) -> bool {
    let mut context = Context::create().unwrap();
    compare_with_context(&mut context, inputs, allowed_off_by_one_bytes, checksum_name, store_if_missing, debug, steps)
}

pub fn compare_with_context(context: &mut Context, inputs: Option<Vec<IoTestEnum>>, allowed_off_by_one_bytes: usize, checksum_name: &str, store_if_missing: bool, debug: bool, mut steps: Vec<s::Node>) -> bool {
    let mut bit = BitmapBgraContainer::empty();
    steps.push(unsafe{ bit.as_mut().get_node()});

    let response = build_steps(context, &steps,inputs.unwrap_or(vec![]), None, debug ).unwrap();

    if let Some(bitmap_key) = unsafe{ bit.bitmap_key(&context) }{

        let mut ctx = ChecksumCtx::visuals();
        ctx.create_if_missing = store_if_missing;

        bitmap_regression_check(&ctx, context, bitmap_key, checksum_name, allowed_off_by_one_bytes)
    }else{
        panic!("execution failed {:?}", response);
    }
}


/// Compares the encoded result of a given job to the known good checksum. If there is a checksum mismatch, a percentage of off-by-one bytes can be allowed.
/// If no good checksum has been stored, pass 'store_if_missing' in order to add it.
/// If you accidentally store a bad checksum, just delete it from the JSON file manually.
///
/// The output io_id is 1
pub fn compare_encoded(input: Option<IoTestEnum>, checksum_name: &str, store_if_missing: bool, debug: bool, require: Constraints, steps: Vec<s::Node>) -> bool {
    compare_encoded_framewise(input, checksum_name, store_if_missing, debug, require, imageflow_types::Framewise::Steps(steps), 1)
}

pub fn compare_encoded_framewise(input: Option<IoTestEnum>, checksum_name: &str, store_if_missing: bool, debug: bool, require: Constraints, framewise: imageflow_types::Framewise, output_count: usize) -> bool {

    let mut io_vec = Vec::new();
    if let Some(i) = input{
        io_vec.push(i);
    }
    let mut output_ids = Vec::new();
    for _ in 0..output_count{
        output_ids.push(io_vec.len() as i32);
        io_vec.push(IoTestEnum::OutputBuffer);
    }


    let mut context = Context::create().unwrap();

    let _ = build_framewise(&mut context, framewise, io_vec, None, debug).unwrap();

    for output_io_id in output_ids{

        let checksum_sub_name = if output_count > 1{
            format!("{checksum_name}_output_{output_io_id}")
        }else{
            checksum_name.to_owned()
        };

        let bytes = context.get_output_buffer_slice(output_io_id).unwrap();

        let mut ctx = ChecksumCtx::visuals();
        ctx.create_if_missing = store_if_missing;
        let result = evaluate_result(&ctx, &checksum_sub_name, ResultKind::Bytes(bytes), require.clone(), true);
        if !result{
            return false;
        }
    }
    return true;

}


pub fn test_with_callback(checksum_name: &str, input: IoTestEnum, callback: fn(&imageflow_types::ImageInfo) -> (Option<imageflow_types::DecoderCommand>, Vec<Node>) ) -> bool{
    let mut context = Context::create().unwrap();
    let matched:bool;

    unsafe {
        IoTestTranslator{}.add(&mut context, 0, input).unwrap();

        let image_info = context.get_unscaled_rotated_image_info(0).unwrap();

        let (tell_decoder, mut steps): (Option<imageflow_types::DecoderCommand>, Vec<Node>) = callback(&image_info);

        if let Some(what) = tell_decoder {
            let send_hints = imageflow_types::TellDecoder001 {
                io_id: 0,
                command: what
            };
            let send_hints_str = serde_json::to_string_pretty(&send_hints).unwrap();
            context.message("v1/tell_decoder", send_hints_str.as_bytes()).1.unwrap();
        }


        let mut bit = BitmapBgraContainer::empty();
        steps.push(bit.as_mut().get_node());

        let send_execute = imageflow_types::Execute001{
            framewise: imageflow_types::Framewise::Steps(steps),
            security: None,
            graph_recording: None
        };
        context.execute_1(send_execute).unwrap();

        let ctx = ChecksumCtx::visuals();
        matched = bitmap_regression_check(&ctx, &context,bit.bitmap_key(&context).unwrap(), checksum_name, 500)
    }
    context.destroy().unwrap();
    matched
}




/// Simplified graph recording configuration
pub fn default_build_config(debug: bool) -> s::Build001Config {
    s::Build001Config{
        security: None,
        graph_recording: if debug {Some(s::Build001GraphRecording::debug_defaults())} else {None},
    }
}

pub fn default_graph_recording(debug: bool) -> Option<imageflow_types::Build001GraphRecording> {
    if debug {Some(s::Build001GraphRecording::debug_defaults())} else {None}

}

/// Simplifies access to raw bitmap data from Imageflow (when using imageflow_types::Node)
/// Consider this an unmovable type. If you move it, you will corrupt the heap.
pub struct BitmapBgraContainer{
    dest_bitmap: BitmapKey,
    _marker: PhantomPinned
}
impl BitmapBgraContainer{
    pub fn empty() -> Pin<Box<Self>>{
        Box::pin(BitmapBgraContainer{
            dest_bitmap: BitmapKey::null(),
            _marker: PhantomPinned
        })
    }
    /// Creates an operation node containing a pointer to self. Do not move self!
    pub unsafe fn get_node(self: Pin<&mut Self>) -> s::Node{
        let key = unsafe {
            let this = self.get_unchecked_mut();
            &mut this.dest_bitmap
        };

        let ptr_to_key = key as *mut BitmapKey;
        s::Node::FlowBitmapKeyPtr { ptr_to_bitmap_key: ptr_to_key as usize}
    }


    pub unsafe fn bitmap_key(&self, _c: &Context) -> Option<BitmapKey>{
        if self.dest_bitmap.is_null() {
            None
        }else {
            Some(self.dest_bitmap)
        }
    }

    /// Returns a reference the bitmap
    /// This reference is only valid for the duration of the context it was created within
    pub fn bitmap_size(&self, c: &Context) -> Option<(usize, usize)>{
        if self.dest_bitmap.is_null(){
            None
        }else {
            Some(c.borrow_bitmaps().unwrap()
                .try_borrow_mut(self.dest_bitmap).unwrap()
                .size())
        }
    }
}
